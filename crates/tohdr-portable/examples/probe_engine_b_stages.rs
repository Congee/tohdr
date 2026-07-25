//! Where does Engine B's time actually go at 60 MP?
//!
//! `docs/engine-comparison.md` reports Engine B at 9403.8 ms / 6.4 MP/s for the
//! 60.22 MP source, against Engine A's 572.3 ms — and concludes Engine B "falls
//! off a cliff". But `spikes/hpvca-probe 9504` shows raw hpvca doing the same
//! pixel count in 1.55 s with `TilesWpp` (38.85 MP/s), and 9.82 s only with
//! `ParallelismStrategy::Single`. Those two facts cannot both describe a codec
//! at its limit, so the cost has to be somewhere in how we call it.
//!
//! This times the four stages of `PortableEngine::encode` separately, plus the
//! two buffer conversions that `codec.rs` performs before handing anything to
//! hpvca. It replicates `codec.rs`'s calls rather than going through
//! `PortableEngine` so each hpvca invocation can be timed on its own; keep the
//! configs here in sync with `config_for`.
//!
//! Run: `cargo run --release --example probe_engine_b_stages -p tohdr-portable`

use std::time::Instant;

use hpvca::{BitDepth, ChromaFormat, EncodeConfig, ParallelismStrategy, Yuv};
use tohdr_core::{GainPlane, Rgb};

/// Mirrors `codec::config_for`.
fn config_for(quality: u8, chroma: ChromaFormat) -> EncodeConfig {
    EncodeConfig::default()
        .with_quality(quality.clamp(1, 100))
        .with_parallelism(ParallelismStrategy::TilesWpp)
        .with_chroma(chroma)
}

/// Same diagonal-gradient-plus-hot-corner scene the codec tests and the hpvca
/// spike use, so the compressibility is comparable across all three.
fn scene_rgb(w: u32, h: u32) -> Vec<u16> {
    let mut v = Vec::with_capacity((w as usize) * (h as usize) * 3);
    for y in 0..h {
        for x in 0..w {
            let fx = x as f32 / w as f32;
            let fy = y as f32 / h as f32;
            let d = ((fx - 0.8).powi(2) + (fy - 0.2).powi(2)).sqrt();
            let hot = (1.0 - (d * 4.0).min(1.0)).powi(2);
            v.push((fx * 200.0 + hot * 55.0).min(255.0) as u16);
            v.push(((fx + fy) * 100.0 + hot * 100.0).min(255.0) as u16);
            v.push((fy * 220.0 + hot * 35.0).min(255.0) as u16);
        }
    }
    v
}

fn scene_gray(w: u32, h: u32) -> Vec<u8> {
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

fn row(label: &str, ms: f64, total_px: u32, bytes: Option<usize>) {
    let mps = total_px as f64 / 1e6 / (ms / 1000.0);
    match bytes {
        Some(b) => println!("  {label:38} {ms:8.1} ms  {mps:7.2} MP/s  {b:>9} B"),
        None => println!("  {label:38} {ms:8.1} ms  {mps:7.2} MP/s"),
    }
}

fn main() {
    // The shape docs/engine-comparison.md benches: 9504x6336, and the gain
    // plane at `DeriveOptions::default()` — subsample 1, i.e. full resolution,
    // which is what `bench` uses and `convert` does not.
    let (w, h) = (9504u32, 6336u32);
    let px = w * h;
    println!("Engine B stage breakdown, {w}x{h} ({:.2} MP), quality 85\n", px as f64 / 1e6);

    let base = Rgb { width: w, height: h, bits: 8, data: scene_rgb(w, h) };
    let gain = GainPlane { width: w, height: h, data: scene_gray(w, h) };

    // --- base plane, exactly as codec::encode_base_heic does it ---
    let t = Instant::now();
    let packed: Vec<u8> = base.data.iter().map(|&v| v as u8).collect();
    let narrow_ms = t.elapsed().as_secs_f64() * 1000.0;
    row("base: u16->u8 narrowing copy", narrow_ms, px, Some(packed.len()));

    let cfg = config_for(85, ChromaFormat::Yuv420);
    let t = Instant::now();
    let base_heic = hpvca::encode_rgb(&packed, w, h, &cfg).expect("base encode");
    let base_ms = t.elapsed().as_secs_f64() * 1000.0;
    row("base: hpvca encode_rgb TilesWpp", base_ms, px, Some(base_heic.len()));
    drop(packed);

    // --- gain plane, exactly as codec::encode_gain_heic does it ---
    let t = Instant::now();
    let y: Vec<u16> = gain.data.iter().map(|&v| v as u16).collect();
    let widen_ms = t.elapsed().as_secs_f64() * 1000.0;
    row("gain: u8->u16 widening copy", widen_ms, px, Some(y.len() * 2));

    let yuv = Yuv::from_planes(
        y,
        Vec::new(),
        Vec::new(),
        gain.width,
        gain.height,
        ChromaFormat::Monochrome,
        BitDepth::Eight,
    )
    .expect("mono yuv");
    let cfg_mono = config_for(85, ChromaFormat::Monochrome);
    let t = Instant::now();
    let gain_heic = hpvca::encode_yuv(&yuv, &cfg_mono).expect("gain encode");
    let gain_ms = t.elapsed().as_secs_f64() * 1000.0;
    row("gain: hpvca encode_yuv TilesWpp", gain_ms, px, Some(gain_heic.len()));

    // The comparison that matters: the same plane through the entry point
    // codec.rs deliberately avoids, and through the default strategy.
    let t = Instant::now();
    let g2 = hpvca::encode_gray(&gain.data, gain.width, gain.height, &cfg_mono).expect("gray");
    let gray_ms = t.elapsed().as_secs_f64() * 1000.0;
    row("gain: hpvca encode_gray (grids!)", gray_ms, px, Some(g2.len()));

    for (name, strat) in [
        ("Single", ParallelismStrategy::Single),
        ("Wpp", ParallelismStrategy::Wpp),
        ("TilesWpp", ParallelismStrategy::TilesWpp),
    ] {
        let c = EncodeConfig::default()
            .with_quality(85)
            .with_parallelism(strat)
            .with_chroma(ChromaFormat::Monochrome);
        let yuv2 = Yuv::from_planes(
            gain.data.iter().map(|&v| v as u16).collect(),
            Vec::new(),
            Vec::new(),
            gain.width,
            gain.height,
            ChromaFormat::Monochrome,
            BitDepth::Eight,
        )
        .expect("mono yuv");
        let t = Instant::now();
        let r = hpvca::encode_yuv(&yuv2, &c).expect("gain encode");
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        row(&format!("  gain via encode_yuv, {name}"), ms, px, Some(r.len()));
    }

    // --- remux: parse both single-item HEICs back out, then mux ---
    let t = Instant::now();
    let bf = tohdr_heif::HeifFile::parse(&base_heic).expect("parse base");
    let bi = bf.primary_item().expect("base primary");
    let base_coded = bf.coded_image(bi).expect("base coded");
    let gf = tohdr_heif::HeifFile::parse(&gain_heic).expect("parse gain");
    let gi = gf.primary_item().expect("gain primary");
    let gain_coded = gf.coded_image(gi).expect("gain coded");
    let parse_ms = t.elapsed().as_secs_f64() * 1000.0;
    row("remux: parse + extract both items", parse_ms, px, None);

    let meta = tohdr_core::GainMapMeta::default();
    let req = tohdr_heif::MuxRequest {
        base: base_coded,
        gain: gain_coded,
        meta,
        flavor: tohdr_core::Flavor::default(),
        base_colour: None,
        tmap_colour: None,
        exif: None,
        xmp: None,
        clli: None,
    };
    let t = Instant::now();
    let out = tohdr_heif::mux(&req).expect("mux");
    let mux_ms = t.elapsed().as_secs_f64() * 1000.0;
    row("remux: mux", mux_ms, px, Some(out.len()));

    let engine_total = narrow_ms + base_ms + widen_ms + gain_ms + parse_ms + mux_ms;
    println!("\n  {:38} {engine_total:8.1} ms  ({:.2} MP/s over base+gain)",
        "TOTAL (what bench measures)", 2.0 * px as f64 / 1e6 / (engine_total / 1000.0));
}
