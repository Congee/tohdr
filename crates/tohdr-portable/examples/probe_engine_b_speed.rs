//! Can Engine B reach Engine A's encode time at 60 MP?
//!
//! Engine A encodes the 60.22 MP base + gain in ~590 ms (VideoToolbox, i.e. the
//! Apple Silicon media block). `probe_engine_b_stages` established that ~98% of
//! Engine B's time is inside hpvca and essentially none of it is our muxer, so
//! the only levers that matter are the ones we hand hpvca.
//!
//! This sweeps them:
//!   * `sao` — hpvca's own docs say SAO "requires an analysis encode before the
//!     final encode, so disabling it nearly halves the transform/RDO work at a
//!     small compression-efficiency cost". Default is on.
//!   * `variance_boost` — extra per-CTU analysis, on by default.
//!   * base and gain concurrently — they are independent encodes run
//!     back-to-back today, and neither saturates ten cores on its own.
//!
//! Sizes are printed alongside every timing because the first two levers buy
//! speed with bits, and a speed win that quietly doubles the file is not a win.
//!
//! Run: `cargo run --release --example probe_engine_b_speed -p tohdr-portable`

use std::time::Instant;

use hpvca::{BitDepth, ChromaFormat, EncodeConfig, ParallelismStrategy, Speed, VarianceBoost, Yuv};

/// Engine A's measured encode for the same 60.22 MP base + gain, from
/// `cargo run --release --example profile -p tohdr-apple`.
const ENGINE_A_MS: f64 = 590.0;

fn scene_rgb8(w: u32, h: u32) -> Vec<u8> {
    let mut v = Vec::with_capacity((w as usize) * (h as usize) * 3);
    for y in 0..h {
        for x in 0..w {
            let fx = x as f32 / w as f32;
            let fy = y as f32 / h as f32;
            let d = ((fx - 0.8).powi(2) + (fy - 0.2).powi(2)).sqrt();
            let hot = (1.0 - (d * 4.0).min(1.0)).powi(2);
            v.push((fx * 200.0 + hot * 55.0).min(255.0) as u8);
            v.push(((fx + fy) * 100.0 + hot * 100.0).min(255.0) as u8);
            v.push((fy * 220.0 + hot * 35.0).min(255.0) as u8);
        }
    }
    v
}

fn scene_gray8(w: u32, h: u32) -> Vec<u8> {
    let mut v = Vec::with_capacity((w as usize) * (h as usize));
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

fn mono_yuv(gray: &[u8], w: u32, h: u32) -> Yuv {
    Yuv::from_planes(
        gray.iter().map(|&v| v as u16).collect(),
        Vec::new(),
        Vec::new(),
        w,
        h,
        ChromaFormat::Monochrome,
        BitDepth::Eight,
    )
    .expect("mono yuv")
}

/// `config_for` from `codec.rs`, plus the knobs under test.
fn cfg(chroma: ChromaFormat, sao: bool, vb: bool) -> EncodeConfig {
    let c = EncodeConfig::default()
        .with_quality(85)
        .with_parallelism(ParallelismStrategy::TilesWpp)
        .with_chroma(chroma)
        .with_sao(sao);
    if vb {
        c
    } else {
        // strength 0 disables variance boost entirely.
        c.with_variance_boost(VarianceBoost::default().octile, 0.0, false)
    }
}

fn main() {
    let (w, h) = (9504u32, 6336u32);
    let px = w * h;
    println!(
        "Engine B speed levers, {w}x{h} ({:.2} MP) base + full-res gain, q85\n\
         Engine A reference: {ENGINE_A_MS:.0} ms for the same pair\n",
        px as f64 / 1e6
    );

    let rgb = scene_rgb8(w, h);
    let gray = scene_gray8(w, h);

    println!("  {:<44} {:>9}  {:>9}  {:>10}", "config", "base ms", "gain ms", "bytes");

    let mut best_sequential = f64::MAX;
    for (label, sao, vb) in [
        ("baseline (sao on, vb on) — today", true, true),
        ("sao off", false, true),
        ("vb off", true, false),
        ("sao off + vb off", false, false),
    ] {
        let cb = cfg(ChromaFormat::Yuv420, sao, vb);
        let t = Instant::now();
        let base = hpvca::encode_rgb(&rgb, w, h, &cb).expect("base");
        let base_ms = t.elapsed().as_secs_f64() * 1000.0;

        let cg = cfg(ChromaFormat::Monochrome, sao, vb);
        let yuv = mono_yuv(&gray, w, h);
        let t = Instant::now();
        let gain = hpvca::encode_yuv(&yuv, &cg).expect("gain");
        let gain_ms = t.elapsed().as_secs_f64() * 1000.0;

        let total = base_ms + gain_ms;
        best_sequential = best_sequential.min(total);
        println!(
            "  {label:<44} {base_ms:>9.1}  {gain_ms:>9.1}  {:>10}   total {total:>8.1} ms  ({:.1}x Engine A)",
            base.len() + gain.len(),
            total / ENGINE_A_MS
        );
    }

    // Base and gain are independent. Today `PortableEngine::encode` runs them
    // back to back; neither saturates ten cores alone, so overlapping them
    // should cost less than their sum.
    println!("\n  concurrent base+gain (std::thread::scope):");
    for (label, sao, vb) in [
        ("  sao on  + vb on", true, true),
        ("  sao off + vb off", false, false),
    ] {
        let cb = cfg(ChromaFormat::Yuv420, sao, vb);
        let cg = cfg(ChromaFormat::Monochrome, sao, vb);
        let yuv = mono_yuv(&gray, w, h);
        let t = Instant::now();
        let (base, gain) = std::thread::scope(|s| {
            let bh = s.spawn(|| hpvca::encode_rgb(&rgb, w, h, &cb).expect("base"));
            let gh = s.spawn(|| hpvca::encode_yuv(&yuv, &cg).expect("gain"));
            (bh.join().unwrap(), gh.join().unwrap())
        });
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        println!(
            "  {label:<44} {:>9}  {:>9}  {:>10}   total {ms:>8.1} ms  ({:.1}x Engine A)",
            "-", "-",
            base.len() + gain.len(),
            ms / ENGINE_A_MS
        );
    }

    // Apple ships the gain plane at half resolution; `tohdr convert` defaults to
    // --gain-subsample 2 while `bench` uses full res. This is what the CLI
    // actually encodes, and it is a quarter of the gain pixels.
    println!("\n  as `convert` actually runs it (gain at subsample 2):");
    let (gw, gh_) = (w / 2, h / 2);
    let gray_half = scene_gray8(gw, gh_);
    for (label, sao, vb) in [
        ("  sao on  + vb on", true, true),
        ("  sao off + vb off", false, false),
    ] {
        let cb = cfg(ChromaFormat::Yuv420, sao, vb);
        let cg = cfg(ChromaFormat::Monochrome, sao, vb);
        let yuv = mono_yuv(&gray_half, gw, gh_);
        let t = Instant::now();
        let (base, gain) = std::thread::scope(|s| {
            let bh = s.spawn(|| hpvca::encode_rgb(&rgb, w, h, &cb).expect("base"));
            let gh2 = s.spawn(|| hpvca::encode_yuv(&yuv, &cg).expect("gain"));
            (bh.join().unwrap(), gh2.join().unwrap())
        });
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        println!(
            "  {label:<44} {:>9}  {:>9}  {:>10}   total {ms:>8.1} ms  ({:.1}x Engine A)",
            "-", "-",
            base.len() + gain.len(),
            ms / ENGINE_A_MS
        );
    }

    // Speed::Slow is the other direction — recorded so the default is known to
    // be the fast one and not left as an open question.
    let cslow = cfg(ChromaFormat::Yuv420, false, false);
    let cslow = EncodeConfig { speed: Speed::Slow, ..cslow };
    let t = Instant::now();
    let b = hpvca::encode_rgb(&rgb, w, h, &cslow).expect("base");
    println!(
        "\n  base only, Speed::Slow + sao off + vb off: {:.1} ms, {} B",
        t.elapsed().as_secs_f64() * 1000.0,
        b.len()
    );

    // Everything above is the synthetic scene, which is smooth and cheap to
    // code. Pass a real photograph to see what actually governs: hand it a
    // 16-bit TIFF and it runs the same base encode on real detail, sweeping
    // quality, so the cost of buying time with bits is explicit.
    let Some(path) = std::env::args().nth(1) else {
        println!("\n  (pass a 16-bit TIFF to sweep quality on real photo content)");
        return;
    };
    let hdr = tohdr_portable::load_hdr(std::path::Path::new(&path)).expect("load");
    let white = hdr.peak_luma(0.001);
    let real = tohdr_core::hdr::ToneMap::Reinhard { white }.to_sdr(&hdr);
    let (rw, rh) = (real.width, real.height);
    let packed: Vec<u8> = real.data.iter().map(|&v| v as u8).collect();
    drop(hdr);
    println!(
        "\n  real photo base encode, {rw}x{rh} ({:.2} MP), sao off:",
        (rw as f64 * rh as f64) / 1e6
    );
    for q in [85u8, 70, 50, 30] {
        let c = EncodeConfig::default()
            .with_quality(q)
            .with_parallelism(ParallelismStrategy::TilesWpp)
            .with_chroma(ChromaFormat::Yuv420)
            .with_sao(false);
        let t = Instant::now();
        let out = hpvca::encode_rgb(&packed, rw, rh, &c).expect("base");
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        println!(
            "    q{q:<3} {ms:>8.1} ms  {:>10} B  ({:.1}x Engine A's {ENGINE_A_MS:.0} ms)",
            out.len(),
            ms / ENGINE_A_MS
        );
    }
}
