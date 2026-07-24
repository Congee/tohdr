//! Spike: is `hpvca` a viable HEVC/HEIC substrate for Engine B?
//!
//! Gate question: does its output actually decode in Apple ImageIO? A pure-Rust
//! HEIC nobody can open is worthless to us. Correctness first, then cost.
//!
//! Run: cargo run -p hpvca-probe --release -- [long_edge]

use hpvca::{ChromaFormat, EncodeConfig, ParallelismStrategy};
use std::time::Instant;

/// Synthetic scene with a bright specular corner, so there is real HDR-ish
/// structure to compress rather than a flat ramp.
fn scene_rgb8(w: u32, h: u32) -> Vec<u8> {
    let mut v = Vec::with_capacity((w * h * 3) as usize);
    for y in 0..h {
        for x in 0..w {
            let fx = x as f32 / w as f32;
            let fy = y as f32 / h as f32;
            // diagonal gradient + a hot radial blob near the top-right
            let d = ((fx - 0.8).powi(2) + (fy - 0.2).powi(2)).sqrt();
            let hot = (1.0 - (d * 4.0).min(1.0)).powi(2);
            let r = (fx * 200.0 + hot * 55.0).min(255.0);
            let g = ((fx + fy) * 100.0 + hot * 100.0).min(255.0);
            let b = (fy * 220.0 + hot * 35.0).min(255.0);
            v.extend_from_slice(&[r as u8, g as u8, b as u8]);
        }
    }
    v
}

fn scene_rgb10(w: u32, h: u32) -> Vec<u16> {
    scene_rgb8(w, h).iter().map(|&b| (b as u16) << 2).collect()
}

/// Stand-in for a gain-map plane: single channel, smooth, low entropy.
fn scene_gray8(w: u32, h: u32) -> Vec<u8> {
    let mut v = Vec::with_capacity((w * h) as usize);
    for y in 0..h {
        for x in 0..w {
            let fx = x as f32 / w as f32;
            let fy = y as f32 / h as f32;
            let d = ((fx - 0.8).powi(2) + (fy - 0.2).powi(2)).sqrt();
            v.push(((1.0 - (d * 3.0).min(1.0)) * 255.0) as u8);
        }
    }
    v
}

fn emit(label: &str, path: &str, bytes: Result<Vec<u8>, hpvca::EncodeError>, dt: f64, px: u32) {
    match bytes {
        Ok(b) => {
            std::fs::write(path, &b).expect("write");
            let mp = px as f64 / 1e6;
            println!(
                "  {label:34} {dt:7.2}s  {:>9}  {:>8.2} MP/s  -> {path}",
                format!("{:.1} KB", b.len() as f64 / 1024.0),
                mp / dt,
            );
        }
        Err(e) => println!("  {label:34} {dt:7.2}s  FAILED: {e:?}"),
    }
}

fn main() {
    let long_edge: u32 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(512);
    let (w, h) = (long_edge, long_edge * 2 / 3);
    let px = w * h;
    std::fs::create_dir_all("out").expect("mkdir out");
    println!("hpvca {w}x{h} ({:.1} MP)", px as f64 / 1e6);

    let rgb8 = scene_rgb8(w, h);
    let rgb10 = scene_rgb10(w, h);
    let gray8 = scene_gray8(w, h);

    // 1. baseline 8-bit 4:2:0 — the "does it open at all" case
    let cfg = EncodeConfig::default().with_quality(90);
    let t = Instant::now();
    let r = hpvca::encode_rgb(&rgb8, w, h, &cfg);
    emit("8bit rgb 420 q90", "out/base8_420.heic", r, t.elapsed().as_secs_f64(), px);

    // 2. 10-bit — what an HDR base image actually needs
    let t = Instant::now();
    let r = hpvca::encode_rgb10(&rgb10, w, h, &cfg);
    emit("10bit rgb 420 q90", "out/base10_420.heic", r, t.elapsed().as_secs_f64(), px);

    // 3. 10-bit 4:4:4 — no chroma loss on the base
    let cfg444 = EncodeConfig::default().with_quality(90).with_chroma(ChromaFormat::Yuv444);
    let t = Instant::now();
    let r = hpvca::encode_rgb10(&rgb10, w, h, &cfg444);
    emit("10bit rgb 444 q90", "out/base10_444.heic", r, t.elapsed().as_secs_f64(), px);

    // 4. monochrome — the gain-map plane. Apple's is 1-channel.
    let t = Instant::now();
    let r = hpvca::encode_gray(&gray8, w, h, &cfg);
    emit("8bit gray (gainmap plane)", "out/gainmap8.heic", r, t.elapsed().as_secs_f64(), px);

    // 5. half-res gain map, as Apple ships it
    let (gw, gh) = (w / 2, h / 2);
    let ghalf = scene_gray8(gw, gh);
    let t = Instant::now();
    let r = hpvca::encode_gray(&ghalf, gw, gh, &cfg);
    emit("8bit gray half-res", "out/gainmap8_half.heic", r, t.elapsed().as_secs_f64(), gw * gh);

    // 6. Parallelism strategy matters structurally, not just for speed: the Grid*
    //    strategies emit a HEIF grid (many tile items + a grid derived item),
    //    which a gain-map remuxer would have to thread a `tmap` through. The
    //    single-image strategies are far cheaper to remux.
    println!("\nstrategy sweep (10-bit 420) — item count decides remux complexity:");
    for (name, strat) in [
        ("Single", ParallelismStrategy::Single),
        ("Wpp", ParallelismStrategy::Wpp),
        ("TilesWpp", ParallelismStrategy::TilesWpp),
        ("Grid", ParallelismStrategy::Grid),
        ("GridWpp", ParallelismStrategy::GridWpp),
    ] {
        let c = EncodeConfig::default().with_quality(90).with_parallelism(strat);
        let t = Instant::now();
        let r = hpvca::encode_rgb10(&rgb10, w, h, &c);
        let path = format!("out/strat_{}.heic", name.to_lowercase());
        emit(&format!("10bit {name}"), &path, r, t.elapsed().as_secs_f64(), px);
    }

    println!("\nnow verify these decode in Apple ImageIO (sips / CGImageSource)");
}
