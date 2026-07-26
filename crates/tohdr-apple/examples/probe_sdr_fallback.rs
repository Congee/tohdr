//! What a device with no HDR support actually shows.
//!
//! A gain-map HEIC's base image *is* the SDR rendition, and a decoder that knows
//! nothing about gain maps shows it unmodified — so "will it look washed out on
//! an SDR device" is not a question about the gain map at all. It is a question
//! about the tone map that produced the base, and it is measurable without a
//! second device: `load_sdr` asks ImageIO for the base rendition, which is the
//! same pixels an SDR viewer gets.
//!
//! Reported per file, on luma in *display* (sRGB-encoded) units, because that is
//! the axis the eye judges flatness on:
//!
//! - `mean`, `p5`, `p50`, `p95` — where the tones sit.
//! - `spread` = p95 - p5, and `rms` = standard deviation. Both fall when an
//!   image is flat. "Washed out" is exactly this pair dropping.
//! - `sat` / `sat32` — mean `(max - min) / max`, over non-black pixels and over
//!   pixels at or above 32. Falls when highlights are pushed toward white.
//! - `clip%` — pixels with any channel at 255. What `--tone-map clip` spends.
//!
//! The first file is the reference; every later one is also diffed against it
//! per pixel (mean and worst absolute luma difference, in 0..255 units), which
//! is the number that says whether an SDR viewer sees the same photograph.
//!
//! Feeding it a `.png` decoded by something other than ImageIO is how the
//! cross-decoder check below was run: `magick x.heic -depth 8 PNG24:x.png`
//! decodes through libheif, which is the class of decoder an SDR-only viewer
//! actually has.
//!
//! # Measured
//!
//! ## From a plain HDR source, where we render the base ourselves
//!
//! `IMG_4913.HEIC` 5712x4284 (24.5 MP; its gain plane is 2856x2142, exactly
//! half), reference = Apple's own base, ours via `tohdr convert --engine apple`.
//! `magick` rows are the same file through libheif:
//!
//! | base | mean | p95 | spread | rms | sat32 | clip% | dmean | dmax |
//! |---|---|---|---|---|---|---|---|---|
//! | IMG_4913, ImageIO | 98.6 | 197 | 187 | 64.8 | 0.264 | 1.93% | - | - |
//! | IMG_4913, libheif | 98.6 | 197 | 187 | 64.8 | 0.263 | 2.33% | 0.15 | 7.60 |
//! | ours `reinhard`, ImageIO | 100.8 | 201 | 190 | 64.4 | 0.260 | 2.65% | 3.15 | 44.45 |
//! | ours `reinhard`, libheif | 100.8 | 201 | 190 | 64.4 | 0.260 | 3.92% | 3.12 | 44.45 |
//! | ours `clip`, ImageIO | 113.5 | 239 | 228 | 76.5 | 0.246 | 8.45% | 14.89 | 77.21 |
//! | ours `clip`, libheif | 113.5 | 239 | 228 | 76.6 | 0.245 | 9.33% | 14.85 | 76.71 |
//!
//! The default `reinhard` lands 2.2 levels brighter than Apple's own base with
//! 3 more levels of spread, mean difference 3.15/255 — the same photograph.
//! `clip` is the one that diverges: 15 levels brighter, 41 more of spread, and
//! 8.45% of pixels clipped against Apple's 1.93%, because everything above SDR
//! white lands on the ceiling instead of rolling off.
//!
//! **`dmean` and `dmax` alone understate what that 3.15 is.** It is not noise.
//! Decoding both through libheif and differencing per pixel in numpy
//! (24,470,208 px): p50 2.57, p90 5.55, p99 10.37, p99.9 21.93. Bucketed by
//! *reference* luma decile the difference is plainly structured —
//!
//! ```text
//! decile 0 (luma   0-13)  mean 1.10     decile 5 (luma 113-130) mean 3.95
//! decile 1 (luma  13-20)  mean 1.41     decile 6 (luma 130-140) mean 2.73
//! decile 2 (luma  20-40)  mean 2.04     decile 7 (luma 140-151) mean 1.72
//! decile 3 (luma  40-80)  mean 3.78     decile 8 (luma 151-187) mean 2.63
//! decile 4 (luma  80-113) mean 4.68     decile 9 (luma 187-255) mean 5.72
//! ```
//!
//! — and spatially clustered: of the pixels above p99.9, **92.1%** have at
//! least one above-threshold 4-neighbour, against **0.4%** expected at that
//! density if the difference were independent per-pixel noise. Those pixels sit
//! at reference luma 242 on average (p5 226), and the 15 worst are all at
//! 242-255, i.e. against white.
//!
//! Note the shape is *two humps*, not a monotonic climb into the highlights:
//! decile 4's 4.68 nearly matches decile 9's 5.72, with a trough at decile 7.
//! That is a tone curve differing across its whole length, not only in the
//! rolloff — consistent with Reinhard vs whatever Apple applies, and not
//! something a "3.15/255 mean" conveys.
//!
//! It does not change the answer: 0.1% of pixels, concentrated against white,
//! and in the direction of *less* clipping than Apple (2.65% vs 1.93%). But
//! "the same photograph" rests on the distribution, not on the mean, so the
//! percentiles above are the honest summary.
//!
//! ## From a Lightroom HDR export, where Lightroom rendered the base
//!
//! `a_srgb.tiff` (LrC "HDR sRGB", 9202x6135), reference = its own IFD0:
//!
//! | base | mean | p95 | spread | rms | sat | sat32 | dmean | dmax |
//! |---|---|---|---|---|---|---|---|---|
//! | Lightroom's SDR rendition | 40.1 | 123 | 120 | 36.8 | 0.609 | 0.605 | - | - |
//! | ours, ImageIO | 40.1 | 122 | 118 | 36.6 | 0.693 | 0.622 | 1.59 | 29.79 |
//! | ours, libheif | 40.0 | 122 | 119 | 36.6 | 0.677 | 0.615 | 1.61 | 31.21 |
//!
//! Here the tone map never runs: the source carries its own gain map, so the
//! pipeline transcodes it and keeps Lightroom's SDR rendition verbatim.
//! `--tone-map clip` produces a **byte-identical** file, which is the proof.
//! 1.59/255 is HEVC quality-85 noise.
//!
//! The `sat` jump, 0.609 -> 0.693, is the metric and not the image: it shrinks
//! to 0.017 under `sat32`, because this frame's median is 30 and `(hi-lo)/hi`
//! is meaningless that close to black. Neither `mean` nor `spread` moves.
//!
//! ## What this settles
//!
//! An SDR-only device shows the base, and the base is a real SDR photograph on
//! both paths. Nothing washes out. Two independent decoders agree to
//! 0.15/255 on Apple's file and 1.6/255 on ours, which also means the base's
//! all-`Unspecified` nclx (see below) is not causing a cross-decoder split —
//! both fall back to sRGB.
//!
//! Worth knowing anyway, and stated carefully because an earlier draft of this
//! paragraph got it wrong in both directions. Walking `iprp`/`ipco`/`ipma` and
//! reporting `colr` per item (`item 46` is `pitm` in both files):
//!
//! | | our output | `IMG_4913.HEIC` |
//! |---|---|---|
//! | base, item 46 | `colr/nclx`, primaries *Unspecified*, transfer *Unspecified*, matrix BT.601, full range | `colr/prof`, 536 B, `"Display P3"` — and **no `nclx` at all** |
//! | `tmap` item | 66: `colr/nclx`, BT.2020 primaries, PQ transfer, matrix 9 | 122: `colr/prof`, 26,664 B, `"Display P3 Primaries; PQ (Adaptive Gain Curve …)"` |
//! | items carrying any ICC | **0** | **100** (every base tile, the base grid, the alt tiles, the `tmap`) |
//!
//! Two corrections to what this file used to claim. Apple's base is *not*
//! "equally Unspecified in its nclx" — it has no `nclx` box, only the ICC, so
//! the two files declare their base colour space by entirely different
//! mechanisms rather than one merely adding a profile on top. And the gap is
//! not one profile against zero: `IMG_4913` attaches an ICC to a hundred items.
//!
//! Conversely, we are less silent than "no ICC profile at all" suggests: our
//! `tmap` does declare BT.2020/PQ via `nclx`. It is the *base* that says
//! nothing — and the base is the item an SDR-only decoder reads, which is why
//! this belongs in this file. The guess both measured decoders make (sRGB) is
//! right today; it is still a declaration the base should be making and isn't.
//! The 26,664-byte `tmap` profile is the one committed as
//! `assets/fixtures/img4913_tmap_icc_profile.icc` — byte count matches.

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
        if let Some((_, r, rw, rh)) = &reference {
            if *rw == rgb.width && *rh == rgb.height {
                let (mut sum, mut worst) = (0.0f64, 0.0f64);
                for (a, b) in r.luma.iter().zip(s.luma.iter()) {
                    let d = (*a as f64 - *b as f64).abs();
                    sum += d;
                    worst = worst.max(d);
                }
                dmean = sum / r.luma.len() as f64;
                dmax = worst;
            }
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
