//! Real-size verification run for Engine B's codec path (see
//! `crates/tohdr-portable/src/codec.rs`). Not a test — a manual probe that
//! writes files to `out/` and prints measured numbers, mirroring
//! `spikes/hpvca-probe` but at the codec module's actual config (TilesWpp,
//! monochrome-via-`encode_yuv` gain plane).
//!
//! Run: cargo run -p tohdr-portable --release --example portable_probe

use hpvca::{BitDepth, ChromaFormat, EncodeConfig, ParallelismStrategy, Yuv};
use std::time::Instant;

fn scene_rgb8(w: u32, h: u32) -> Vec<u8> {
    let mut v = Vec::with_capacity((w * h * 3) as usize);
    for y in 0..h {
        for x in 0..w {
            let fx = x as f32 / w as f32;
            let fy = y as f32 / h as f32;
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

fn main() {
    std::fs::create_dir_all("out").expect("mkdir out");

    // 12MP-ish, a plausible real photo long edge (Lightroom Classic default
    // export is often 4:3 or 3:2; 4032x3024 matches a common phone sensor).
    let (w, h) = (4032u32, 3024u32);
    let (gw, gh) = (w / 2, h / 2);
    println!("base {w}x{h} ({:.1} MP), gain {gw}x{gh} ({:.1} MP)", (w * h) as f64 / 1e6, (gw * gh) as f64 / 1e6);

    let rgb10: Vec<u16> = scene_rgb8(w, h).into_iter().map(|b| (b as u16) << 2).collect();
    let gray: Vec<u8> = scene_gray8(gw, gh);

    // Base: same config as codec::encode_base_heic at base_quality=85 (below
    // the 4:4:4 threshold, so 4:2:0).
    let base_cfg = EncodeConfig::default()
        .with_quality(85)
        .with_parallelism(ParallelismStrategy::TilesWpp)
        .with_chroma(ChromaFormat::Yuv420);
    let t = Instant::now();
    let base_heic = hpvca::encode_rgb10(&rgb10, w, h, &base_cfg).expect("base encode");
    let base_dt = t.elapsed().as_secs_f64();
    std::fs::write("out/portable_base10_420.heic", &base_heic).expect("write base");
    println!(
        "base:  {:7.2}s  {:>9}  {:>8.2} MP/s",
        base_dt,
        format!("{:.1} KB", base_heic.len() as f64 / 1024.0),
        (w * h) as f64 / 1e6 / base_dt,
    );

    // Gain: same config as codec::encode_gain_heic at gain_quality=85, routed
    // through encode_yuv with a monochrome Yuv (NOT encode_gray — see
    // codec.rs module docs for why encode_gray would silently grid this).
    let y: Vec<u16> = gray.iter().map(|&b| b as u16).collect();
    let yuv = Yuv::from_planes(y, Vec::new(), Vec::new(), gw, gh, ChromaFormat::Monochrome, BitDepth::Eight)
        .expect("build monochrome Yuv");
    let gain_cfg = EncodeConfig::default()
        .with_quality(85)
        .with_parallelism(ParallelismStrategy::TilesWpp)
        .with_chroma(ChromaFormat::Monochrome);
    let t = Instant::now();
    let gain_heic = hpvca::encode_yuv(&yuv, &gain_cfg).expect("gain encode");
    let gain_dt = t.elapsed().as_secs_f64();
    std::fs::write("out/portable_gain8_mono.heic", &gain_heic).expect("write gain");
    println!(
        "gain:  {:7.2}s  {:>9}  {:>8.2} MP/s",
        gain_dt,
        format!("{:.1} KB", gain_heic.len() as f64 / 1024.0),
        (gw * gh) as f64 / 1e6 / gain_dt,
    );

    // Same probe again but through encode_gray directly, to demonstrate the
    // pitfall codec.rs documents: this should grid at this resolution.
    let t = Instant::now();
    let gain_cfg_gray = EncodeConfig::default()
        .with_quality(85)
        .with_parallelism(ParallelismStrategy::TilesWpp);
    let gain_heic_via_gray = hpvca::encode_gray(&gray, gw, gh, &gain_cfg_gray).expect("gray encode");
    let dt2 = t.elapsed().as_secs_f64();
    std::fs::write("out/portable_gain8_via_encode_gray.heic", &gain_heic_via_gray).expect("write");
    println!(
        "gain (encode_gray, for comparison): {:7.2}s  {:>9}  contains 'grid' fourcc: {}",
        dt2,
        format!("{:.1} KB", gain_heic_via_gray.len() as f64 / 1024.0),
        gain_heic_via_gray.windows(4).any(|s| s == b"grid"),
    );

    println!("\nnow run: python3 <heif_detail.py> out/portable_base10_420.heic");
    println!("         python3 <heif_detail.py> out/portable_gain8_mono.heic");
    println!("         python3 <heif_detail.py> out/portable_gain8_via_encode_gray.heic");
}
