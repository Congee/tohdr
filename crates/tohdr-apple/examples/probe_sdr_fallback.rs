//! What a device with no HDR support actually shows: the base image, since a
//! gain-map-unaware decoder renders it unmodified.
//!
//! Prints luma statistics (mean/percentiles, spread, rms, saturation, clip%) for
//! each file in display units, and diffs every later file against the first per
//! pixel. Feed it a `.png` to compare a non-ImageIO decode.
//!
//! Findings and the tables they came from: docs/sdr-fallback.md.

use std::path::{Path, PathBuf};

fn srgb_luma(px: &[u16]) -> f64 {
    // Rec.709 luma on the *encoded* values, deliberately. This measures how the
    // image looks, not its light output, so it must not linearize first.
    0.2126 * px[0] as f64 + 0.7152 * px[1] as f64 + 0.0722 * px[2] as f64
}

struct Stats {
    mean: f64,
    p5: u16,
    p50: u16,
    p95: u16,
    rms: f64,
    sat: f64,
    sat32: f64,
    clip: f64,
    luma: Vec<f32>,
}

fn stats(rgb: &tohdr_core::Rgb) -> Stats {
    let n = rgb.width as usize * rgb.height as usize;
    let mut hist = [0u64; 256];
    let mut luma = Vec::with_capacity(n);
    let (mut sum, mut sat_sum, mut sat_n, mut clipped) = (0.0f64, 0.0f64, 0u64, 0u64);
    // Reported separately because `(hi - lo) / hi` is unstable near black: at
    // hi = 3 one level of 8-bit or 4:2:0 chroma error moves it by 0.33. On a
    // dark image that noise dominates the metric and reads as a saturation
    // shift the eye cannot see, so `sat32` restricts it to pixels bright
    // enough for the ratio to mean something.
    const SAT_FLOOR: u16 = 32;
    let (mut sat32_sum, mut sat32_n) = (0.0f64, 0u64);
    for px in rgb.data.chunks_exact(3) {
        let y = srgb_luma(px);
        sum += y;
        hist[(y.round() as usize).min(255)] += 1;
        luma.push(y as f32);
        let (lo, hi) = px.iter().fold((u16::MAX, 0u16), |(l, h), v| (l.min(*v), h.max(*v)));
        if hi > 0 {
            sat_sum += (hi - lo) as f64 / hi as f64;
            sat_n += 1;
        }
        if hi >= SAT_FLOOR {
            sat32_sum += (hi - lo) as f64 / hi as f64;
            sat32_n += 1;
        }
        if hi >= 255 {
            clipped += 1;
        }
    }
    let mean = sum / n as f64;
    let pct = |p: f64| -> u16 {
        let target = (n as f64 * p) as u64;
        let mut acc = 0u64;
        for (v, c) in hist.iter().enumerate() {
            acc += c;
            if acc >= target {
                return v as u16;
            }
        }
        255
    };
    let var = luma.iter().map(|y| (*y as f64 - mean).powi(2)).sum::<f64>() / n as f64;
    Stats {
        mean,
        p5: pct(0.05),
        p50: pct(0.50),
        p95: pct(0.95),
        rms: var.sqrt(),
        sat: sat_sum / sat_n.max(1) as f64,
        sat32: sat32_sum / sat32_n.max(1) as f64,
        clip: clipped as f64 / n as f64 * 100.0,
        luma,
    }
}

fn main() {
    let paths: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    if paths.is_empty() {
        eprintln!("usage: probe_sdr_fallback <reference> [comparison ...]");
        std::process::exit(2);
    }
    println!(
        "{:<30} {:>6} {:>4} {:>4} {:>4} {:>7} {:>6} {:>6} {:>6} {:>7}  {:>6} {:>5}",
        "base rendition", "mean", "p5", "p50", "p95", "spread", "rms", "sat", "sat32", "clip%",
        "dmean", "dmax"
    );
    let mut reference: Option<(String, Stats, u32, u32)> = None;
    for path in &paths {
        // Distinguished, because "could not open" for both a missing file and
        // an unreadable one sends you looking for a decoder bug when the real
        // problem is the working directory.
        if !path.exists() {
            println!("{}: no such file", path.display());
            continue;
        }
        let rgb = match tohdr_apple::load_sdr(path) {
            Ok(rgb) => rgb,
            Err(e) => {
                println!("{}: ImageIO could not decode it ({e:?})", path.display());
                continue;
            }
        };
        let name = Path::new(path)
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let s = stats(&rgb);
        let (mut dmean, mut dmax) = (f64::NAN, f64::NAN);
        if let Some((_, r, rw, rh)) = &reference
            && *rw == rgb.width
            && *rh == rgb.height
        {
            let (mut sum, mut worst) = (0.0f64, 0.0f64);
            for (a, b) in r.luma.iter().zip(s.luma.iter()) {
                let d = (*a as f64 - *b as f64).abs();
                sum += d;
                worst = worst.max(d);
            }
            dmean = sum / r.luma.len() as f64;
            dmax = worst;
        }
        let fmt = |v: f64| if v.is_nan() { "-".to_string() } else { format!("{v:.2}") };
        println!(
            "{name:<30} {:>6.1} {:>4} {:>4} {:>4} {:>7} {:>6.1} {:>6.3} {:>6.3} {:>5.2}%  {:>6} {:>5}",
            s.mean,
            s.p5,
            s.p50,
            s.p95,
            s.p95 - s.p5,
            s.rms,
            s.sat,
            s.sat32,
            s.clip,
            fmt(dmean),
            fmt(dmax),
        );
        if reference.is_none() {
            reference = Some((name, s, rgb.width, rgb.height));
        }
    }
}
