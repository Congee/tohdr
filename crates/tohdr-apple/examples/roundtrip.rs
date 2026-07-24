//! `cargo run --release --example roundtrip -p tohdr-apple -- <hdr.tiff>`
//!
//! Encode one source with both engines, decode each result back **through
//! ImageIO with the gain map applied**, and measure how close the
//! reconstruction lands to the original HDR.
//!
//! Without this, comparing the two engines' file sizes is meaningless: the two
//! encoders map their `quality` scales differently, so a smaller file may just
//! be a worse one. Measuring the decoded result is what makes size comparable.
//!
//! The decode side is deliberately ImageIO for both engines — the platform's
//! own reconstruction, not ours, so neither engine is scored by its own reader.

use std::path::{Path, PathBuf};

use tohdr_core::derive::DeriveOptions;
use tohdr_core::encode::{EncodeOptions, GainMapEncoder};
use tohdr_core::hdr::{derive_consistent, ToneMap};
use tohdr_core::HdrRgb;

/// Compare two HDR buffers on pixels bright enough that 8-bit base
/// quantization is not the dominant error (criterion 12's 0.05 linear floor).
struct Cmp {
    worst_rel: f64,
    /// Where the worst pixel is, and how saturated it is — a luma-derived
    /// single-channel map necessarily under-corrects a highlight that clips in
    /// one channel only, so knowing whether the tail sits on such a pixel is
    /// the difference between "the encoder is bad" and "this is the documented
    /// limit of a 1-channel map".
    worst_at: (u32, u32),
    worst_saturation: f64,
    psnr: f64,
    n: usize,
    /// 99.9th-percentile relative error, which ignores the handful of
    /// pathological pixels the worst-case number is dominated by.
    p999_rel: f64,
}

fn compare(src: &HdrRgb, got: &HdrRgb) -> Option<Cmp> {
    if src.width != got.width || src.height != got.height {
        eprintln!(
            "  size mismatch: source {}x{}, decoded {}x{}",
            src.width, src.height, got.width, got.height
        );
        return None;
    }
    let mut worst_rel = 0.0f64;
    let mut worst_at = (0u32, 0u32);
    let mut sse = 0.0f64;
    let mut n = 0usize;
    let mut rels: Vec<f64> = Vec::new();
    for y in 0..src.height {
        for x in 0..src.width {
            let a = src.luma(x, y) as f64;
            let b = got.luma(x, y) as f64;
            if a < 0.05 {
                continue;
            }
            let rel = ((b - a) / a).abs();
            if rel > worst_rel {
                worst_rel = rel;
                worst_at = (x, y);
            }
            rels.push(rel);
            sse += (b - a) * (b - a);
            n += 1;
        }
    }
    if n == 0 {
        return None;
    }
    rels.sort_by(|p, q| p.partial_cmp(q).unwrap());
    let p999_rel = rels[((rels.len() as f64 * 0.999) as usize).min(rels.len() - 1)];

    // Saturation of the worst pixel: 1 - min/max across RGB. Near 1 means the
    // pixel is dominated by a single channel.
    let (wx, wy) = worst_at;
    let px = src.pixel(wx, wy);
    let hi = px[0].max(px[1]).max(px[2]) as f64;
    let lo = px[0].min(px[1]).min(px[2]) as f64;
    let worst_saturation = if hi > 0.0 { 1.0 - lo / hi } else { 0.0 };
    // Peak is the source's own maximum, so PSNR is against the real signal
    // range rather than an assumed 1.0 white.
    let mut peak = 0.0f64;
    for y in 0..src.height {
        for x in 0..src.width {
            peak = peak.max(src.luma(x, y) as f64);
        }
    }
    let mse = sse / n as f64;
    let psnr = if mse <= 0.0 {
        f64::INFINITY
    } else {
        20.0 * (peak / mse.sqrt()).log10()
    };
    Some(Cmp { worst_rel, worst_at, worst_saturation, psnr, n, p999_rel })
}

fn main() {
    let src_path = PathBuf::from(
        std::env::args().nth(1).expect("usage: roundtrip <hdr.tiff>"),
    );
    let outdir = Path::new("out");

    let hdr = tohdr_portable::load_hdr(&src_path).expect("load source");
    let white = hdr.peak_luma(0.001);
    let base = ToneMap::Reinhard { white }.to_sdr(&hdr);
    let (gain, meta) = derive_consistent(&hdr, &base, &DeriveOptions::default());
    let opts = EncodeOptions::default();

    println!(
        "source {}x{}, peak {:.3}x ({:.3} stops), declared headroom {:.3} stops",
        hdr.width,
        hdr.height,
        white,
        white.log2(),
        meta.alt_headroom
    );

    let apple = tohdr_apple::AppleEngine;
    let portable = tohdr_portable::PortableEngine;

    let runs: Vec<(&str, Result<Vec<u8>, String>)> = vec![
        (
            "apple-imageio",
            apple.encode(&base, &gain, &meta, &opts).map_err(|e| e.to_string()),
        ),
        (
            "portable-hpvca",
            portable.encode(&base, &gain, &meta, &opts).map_err(|e| e.to_string()),
        ),
    ];

    for (name, res) in runs {
        println!("\n=== {name} ===");
        let bytes = match res {
            Err(e) => {
                println!("  encode failed: {e}");
                continue;
            }
            Ok(b) => b,
        };
        let path = outdir.join(format!("rt_{name}.heic"));
        std::fs::write(&path, &bytes).expect("write");
        println!("  {} bytes -> {}", bytes.len(), path.display());

        match tohdr_apple::inspect_bytes(&bytes) {
            Err(e) => println!("  ImageIO read-back failed: {e}"),
            Ok(rb) => println!(
                "  ImageIO: apple_aux={} iso_aux={} gain={:?} consistent={:?}",
                rb.apple_aux, rb.iso_aux, rb.gain_size, rb.headroom_consistent()
            ),
        }

        match tohdr_apple::load_hdr(&path) {
            Err(e) => println!("  HDR decode failed: {e}"),
            Ok(dec) => {
                let dpeak = dec.peak_luma(0.001);
                println!(
                    "  decoded peak {:.3}x ({:.3} stops) vs source {:.3}x",
                    dpeak,
                    dpeak.log2(),
                    white
                );
                match compare(&hdr, &dec) {
                    None => println!("  (could not compare)"),
                    Some(c) => {
                        println!(
                            "  PSNR {:.2} dB over {} pixels; p99.9 rel err {:.2}%",
                            c.psnr,
                            c.n,
                            c.p999_rel * 100.0
                        );
                        println!(
                            "  worst rel err {:.2}% at ({}, {}), saturation {:.2}",
                            c.worst_rel * 100.0,
                            c.worst_at.0,
                            c.worst_at.1,
                            c.worst_saturation
                        );
                    }
                }
            }
        }
    }
}
