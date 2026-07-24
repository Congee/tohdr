//! Round-trip and edge-case coverage for `tohdr_core::derive`.
//!
//! Test images are synthesized directly (no external crates): a "linear HDR"
//! scene is generated procedurally, tone-mapped down to an SDR base with a
//! simple Reinhard curve (`base = hdr / (1 + hdr)`, applied per channel in
//! linear light), then both are sRGB-encoded into `Rgb` buffers the same way
//! `derive`/`apply` expect. Because `derive` computes its gain ratio directly
//! from the two buffers it's given (not from an assumed tone curve), the
//! tone-map choice only affects how much headroom the test exercises, not the
//! achievable round-trip accuracy.

use tohdr_core::derive::{apply, derive, DeriveOptions};
use tohdr_core::Rgb;

const BITS: u8 = 10;

fn srgb_encode(v: f32) -> f32 {
    let v = v.clamp(0.0, 1.0);
    if v <= 0.0031308 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

fn srgb_decode(v: f32) -> f32 {
    let v = v.clamp(0.0, 1.0);
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

fn max_value() -> u16 {
    (1u32 << BITS) as u16 - 1
}

/// Build an `Rgb` from a per-pixel linear-light generator.
fn make_rgb(w: u32, h: u32, mut f: impl FnMut(u32, u32) -> [f32; 3]) -> Rgb {
    let max = max_value() as f32;
    let mut data = Vec::with_capacity(w as usize * h as usize * 3);
    for y in 0..h {
        for x in 0..w {
            let lin = f(x, y);
            for c in lin {
                data.push((srgb_encode(c) * max).round() as u16);
            }
        }
    }
    Rgb {
        width: w,
        height: h,
        bits: BITS,
        data,
    }
}

/// Reinhard-tonemap an HDR (linear) `Rgb` down to an SDR base, decoding and
/// re-encoding through sRGB around the compression step.
fn tonemap_to_base(hdr: &Rgb) -> Rgb {
    let max = max_value() as f32;
    let data: Vec<u16> = hdr
        .data
        .iter()
        .map(|&s| {
            let lin = srgb_decode(s as f32 / max);
            let mapped = lin / (1.0 + lin);
            (srgb_encode(mapped) * max).round() as u16
        })
        .collect();
    Rgb {
        width: hdr.width,
        height: hdr.height,
        bits: hdr.bits,
        data,
    }
}

fn assert_no_nan(img: &Rgb) {
    for &s in &img.data {
        assert!((s as f32).is_finite(), "non-finite sample in output");
    }
}

/// A gradient HDR scene, bright in the corner opposite the origin. Grayscale
/// (equal on all 3 channels) by design: the gain map this module produces is
/// single-channel/luma-derived (see module docs, "Channel combination"), so
/// it reconstructs achromatic content exactly up to quantization; per-channel
/// divergence (chromatic highlights) is a separate, documented error source,
/// not what this accuracy number is meant to isolate.
fn gradient_hdr(w: u32, h: u32) -> Rgb {
    make_rgb(w, h, |x, y| {
        let u = x as f32 / (w - 1).max(1) as f32;
        let v = y as f32 / (h - 1).max(1) as f32;
        let val = 0.05 + 0.9 * (0.5 * u + 0.5 * v);
        [val, val, val]
    })
}

fn max_mean_error(a: &Rgb, b: &Rgb) -> (f32, f32) {
    assert_eq!(a.data.len(), b.data.len());
    let max = max_value() as f32;
    let mut max_err = 0f32;
    let mut sum_err = 0f32;
    for (&x, &y) in a.data.iter().zip(b.data.iter()) {
        let e = (x as f32 - y as f32).abs() / max;
        max_err = max_err.max(e);
        sum_err += e;
    }
    (max_err, sum_err / a.data.len() as f32)
}

#[test]
fn round_trip_full_res() {
    let hdr = gradient_hdr(32, 32);
    let base = tonemap_to_base(&hdr);

    let opts = DeriveOptions {
        subsample: 1,
        ..Default::default()
    };
    let (gain, meta) = derive(&hdr, &base, &opts);
    assert_no_nan(&base);
    for &g in &gain.data {
        assert!((g as f32).is_finite());
    }
    let reconstructed = apply(&base, &gain, &meta);
    assert_no_nan(&reconstructed);

    let (max_err, mean_err) = max_mean_error(&hdr, &reconstructed);
    println!("round_trip_full_res: max_err={max_err:.6} mean_err={mean_err:.6} (fraction of full scale)");
    assert!(max_err < 0.01, "max error too high: {max_err}");
    assert!(mean_err < 0.005, "mean error too high: {mean_err}");
}

#[test]
fn round_trip_subsampled() {
    let hdr = gradient_hdr(32, 32);
    let base = tonemap_to_base(&hdr);

    let opts = DeriveOptions {
        subsample: 2,
        ..Default::default()
    };
    let (gain, meta) = derive(&hdr, &base, &opts);
    assert_eq!(gain.width, 16);
    assert_eq!(gain.height, 16);
    let reconstructed = apply(&base, &gain, &meta);
    assert_no_nan(&reconstructed);

    let (max_err, mean_err) = max_mean_error(&hdr, &reconstructed);
    println!("round_trip_subsampled: max_err={max_err:.6} mean_err={mean_err:.6} (fraction of full scale)");
    assert!(max_err < 0.03, "max error too high: {max_err}");
    assert!(mean_err < 0.01, "mean error too high: {mean_err}");
}

#[test]
fn edge_case_pure_black() {
    let hdr = make_rgb(8, 8, |_, _| [0.0, 0.0, 0.0]);
    let base = make_rgb(8, 8, |_, _| [0.0, 0.0, 0.0]);
    let (gain, meta) = derive(&hdr, &base, &DeriveOptions::default());
    for &g in &gain.data {
        assert!((g as f32).is_finite());
    }
    let recon = apply(&base, &gain, &meta);
    assert_no_nan(&recon);
    for &s in &recon.data {
        assert_eq!(s, 0, "black should reconstruct to black");
    }
}

#[test]
fn edge_case_pure_white_clipped() {
    let hdr = make_rgb(8, 8, |_, _| [1.0, 1.0, 1.0]);
    let base = make_rgb(8, 8, |_, _| [1.0, 1.0, 1.0]);
    let (gain, meta) = derive(&hdr, &base, &DeriveOptions::default());
    for &g in &gain.data {
        assert!((g as f32).is_finite());
    }
    let recon = apply(&base, &gain, &meta);
    assert_no_nan(&recon);
    let max = max_value();
    for &s in &recon.data {
        assert!(s >= max - 2, "white should reconstruct near-white, got {s}");
    }
}

#[test]
fn edge_case_hot_pixel_on_black_field() {
    let (w, h) = (8u32, 8u32);
    let hdr = make_rgb(w, h, |x, y| {
        if x == 4 && y == 4 {
            [1.8, 1.8, 1.8]
        } else {
            [0.0, 0.0, 0.0]
        }
    });
    let base = tonemap_to_base(&hdr);
    let (gain, meta) = derive(&hdr, &base, &DeriveOptions::default());
    for &g in &gain.data {
        assert!((g as f32).is_finite());
    }
    let recon = apply(&base, &gain, &meta);
    assert_no_nan(&recon);

    let (max_err, _mean_err) = max_mean_error(&hdr, &recon);
    assert!(max_err.is_finite());
    // The hot pixel itself should reconstruct much brighter than its
    // (all-black) neighbors.
    let idx = (4 * w as usize + 4) * 3;
    let neighbor_idx = (4 * w as usize + 3) * 3;
    assert!(recon.data[idx] > recon.data[neighbor_idx]);
}

#[test]
fn edge_case_sdr_only_unity_gain() {
    // hdr == sdr_base: no headroom, gain map should be unity (max_log2 ~ 0).
    let img = gradient_hdr(8, 8);
    let (gain, meta) = derive(&img, &img, &DeriveOptions::default());
    for &g in &gain.data {
        assert!((g as f32).is_finite());
    }
    assert!(
        meta.max_log2[0].abs() < 1e-4,
        "max_log2 should be ~0 for an SDR-only image, got {}",
        meta.max_log2[0]
    );
    assert!(
        meta.min_log2[0].abs() < 1e-4,
        "min_log2 should be ~0 for an SDR-only image, got {}",
        meta.min_log2[0]
    );

    let recon = apply(&img, &gain, &meta);
    assert_no_nan(&recon);
    let (max_err, _) = max_mean_error(&img, &recon);
    assert!(max_err < 0.01, "unity gain round trip should be near-exact: {max_err}");
}

#[test]
fn monotonicity_brighter_hdr_never_yields_smaller_gain() {
    // Constant base; hdr increases monotonically along x. The derived gain
    // plane (subsample 1, so pixel-for-pixel with the image) must be
    // non-decreasing along the same axis.
    let (w, h) = (32u32, 4u32);
    let base = make_rgb(w, h, |_, _| [0.2, 0.2, 0.2]);
    let hdr = make_rgb(w, h, |x, _| {
        let u = x as f32 / (w - 1) as f32;
        let v = 0.05 + 1.5 * u;
        [v, v, v]
    });

    let (gain, _meta) = derive(&hdr, &base, &DeriveOptions::default());
    for y in 0..h {
        let mut prev = gain.data[(y * w) as usize];
        for x in 1..w {
            let g = gain.data[(y * w + x) as usize];
            assert!(
                g >= prev,
                "gain decreased at x={x},y={y}: {prev} -> {g} despite brighter hdr input"
            );
            prev = g;
        }
    }
}
