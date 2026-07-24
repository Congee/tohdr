//! Gain-map derivation: given an HDR source and an SDR base, compute the gain
//! plane and the metadata that reconstructs the HDR image from the base.
//!
//! # Reconstruction equation
//!
//! Per pixel, in linear light, on the luma channel (see "Channel combination"
//! below):
//!
//! ```text
//! ratio       = (alt_linear + alt_offset) / (base_linear + base_offset)
//! gain_log2   = log2(max(ratio, eps))                                    // derive
//! gain_norm   = clamp((gain_log2 - min_log2) / (max_log2 - min_log2)) ^ gamma
//! gain_u8     = round(gain_norm * 255)                                   // stored
//!
//! decoded     = min_log2 + (gain_u8/255) ^ (1/gamma) * (max_log2 - min_log2) // apply
//! alt_linear' = (base_linear + base_offset) * exp2(decoded) - alt_offset
//! ```
//!
//! This is a direct port of libavif's `avifRGBImageComputeGainMap` /
//! `avifRGBImageApplyGainMap` (AOMediaCodec/libavif, `src/gainmap.c`):
//! - ratio + log2 form: `src/gainmap.c:711-713`.
//! - encode gamma (forward `powf(v, gamma)`): `src/gainmap.c:775-781`.
//! - decode gamma (inverse `powf(v, 1/gamma)`, `gammaInv` built from
//!   `1.0f / gamma`): `src/gainmap.c:226-228`.
//! - apply order — `(base + base_offset) * exp2(log2 * weight) - alt_offset`,
//!   offset-then-scale-then-offset, alt_offset subtracted last:
//!   `src/gainmap.c:266-267`. We always fully apply the map (`weight = 1`),
//!   since `apply()` has no partial-headroom target.
//! - headroom sign flip (gain stores log-ratio of alt to base; flip if alt is
//!   darker than base): `src/gainmap.c:718-738`. We store the flip implicitly
//!   by letting `ratio` go negative in log2 space — no explicit renormalize
//!   step is needed since we compute `min_log2`/`max_log2` from the signed
//!   data directly (unlike libavif we skip the outlier-trimming histogram in
//!   `avifFindMinMaxWithoutOutliers`, `src/gainmap.c:375-429` — we use the raw
//!   min/max, which is simpler but more sensitive to a single extreme pixel).
//!
//! # Transfer function
//!
//! [`Rgb`] samples are treated as **sRGB-gamma-encoded** in `0..=max_value`
//! (not linear, not PQ/HLG). We linearize with the true sRGB EOTF (piecewise
//! linear segment + power curve), not a pure 2.2 power curve, because sRGB is
//! the de-facto encoding for 8/10-bit RGB buffers in this codebase and the
//! toe segment matters for the black-pixel edge case (a pure gamma curve
//! divides by zero in its derivative at 0; sRGB's linear toe does not).
//!
//! Both `hdr` and `sdr_base` — and the reconstructed output of [`apply`] — are
//! stored in the *same* `0..=max_value` encoded range. This module does not
//! model extended-range/float HDR storage: an "HDR" input is simply an image
//! whose linear values are brighter than the SDR base at the same encoded
//! bit depth, and reconstruction clamps to `1.0` in linear space before
//! re-encoding. Representing true above-white HDR headroom would need a
//! float or extended-range pixel format, which [`Rgb`] does not have.

use crate::{GainMapMeta, GainPlane, Rgb};

/// Options controlling [`derive`].
#[derive(Clone, Copy, Debug)]
pub struct DeriveOptions {
    /// Gain-plane downsampling factor relative to the base resolution. `1` =
    /// full res, `2` = half res (Apple's convention).
    pub subsample: u32,
    /// Declared log2 headroom of the alternate (HDR) image, stored verbatim
    /// into [`GainMapMeta::alt_headroom`]. Not derived from pixel data: it's
    /// the caller's stated target, analogous to libavif's `hdrHeadroom`
    /// argument to the apply side. `min_log2`/`max_log2`, by contrast, always
    /// come from the actual per-pixel gain range.
    pub alt_headroom: f32,
    /// Added to base samples (linear) before taking the ratio and before
    /// applying gain. Keeps a black base from pinning the ratio to 0.
    pub base_offset: f32,
    /// Subtracted from the reconstructed alternate (linear) after applying
    /// gain.
    pub alt_offset: f32,
    /// Encoding gamma applied to the normalized gain value before
    /// quantization. `1.0` = linear.
    pub gamma: f32,
}

impl Default for DeriveOptions {
    /// Full resolution, ~1/64 offsets (matches libavif's encoding defaults,
    /// `src/gainmap.c:23-24`), gamma 1.0, 2 stops of declared headroom.
    fn default() -> Self {
        Self {
            subsample: 1,
            alt_headroom: 2.0,
            base_offset: 1.0 / 64.0,
            alt_offset: 1.0 / 64.0,
            gamma: 1.0,
        }
    }
}

const EPS: f32 = 1e-6;
// BT.709 luma weights, matching libavif's grayscale conversion for
// single-channel gain maps (`src/gainmap.c:700-704`, `avifColorPrimariesComputeYCoeffs`).
const LUMA: [f32; 3] = [0.2126, 0.7152, 0.0722];

fn srgb_to_linear(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.0031308 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

fn sample_encoded(img: &Rgb, x: u32, y: u32, c: usize) -> f32 {
    let idx = (y as usize * img.width as usize + x as usize) * 3 + c;
    img.data[idx] as f32 / img.max_value() as f32
}

fn luma_linear(img: &Rgb, x: u32, y: u32) -> f32 {
    let r = srgb_to_linear(sample_encoded(img, x, y, 0));
    let g = srgb_to_linear(sample_encoded(img, x, y, 1));
    let b = srgb_to_linear(sample_encoded(img, x, y, 2));
    LUMA[0] * r + LUMA[1] * g + LUMA[2] * b
}

/// Bilinear-sample a (possibly lower-res) gain plane at a base-resolution
/// pixel coordinate, returning a value in `0.0..=1.0`. Bilinear is a
/// reasonable, cheap choice for the up to 2x-subsampled planes this module
/// produces; libavif uses its general `avifImageScale` for the same purpose.
fn sample_gain_bilinear(gain: &GainPlane, x: u32, y: u32, base_w: u32, base_h: u32) -> f32 {
    if gain.width == base_w && gain.height == base_h {
        return gain.data[y as usize * gain.width as usize + x as usize] as f32 / 255.0;
    }
    let sx = (x as f32 + 0.5) * gain.width as f32 / base_w as f32 - 0.5;
    let sy = (y as f32 + 0.5) * gain.height as f32 / base_h as f32 - 0.5;
    let x0f = sx.floor();
    let y0f = sy.floor();
    let fx = sx - x0f;
    let fy = sy - y0f;
    let gw1 = (gain.width as i64 - 1).max(0);
    let gh1 = (gain.height as i64 - 1).max(0);
    let clampi = |v: i64, hi: i64| v.clamp(0, hi);
    let x0 = clampi(x0f as i64, gw1);
    let x1 = clampi(x0f as i64 + 1, gw1);
    let y0 = clampi(y0f as i64, gh1);
    let y1 = clampi(y0f as i64 + 1, gh1);
    let get = |xx: i64, yy: i64| gain.data[yy as usize * gain.width as usize + xx as usize] as f32 / 255.0;
    let top = get(x0, y0) * (1.0 - fx) + get(x1, y0) * fx;
    let bot = get(x0, y1) * (1.0 - fx) + get(x1, y1) * fx;
    top * (1.0 - fy) + bot * fy
}

/// Derive a gain plane plus reconstruction metadata from an HDR image and the
/// SDR base that will ship alongside it. See the module docs for the
/// equation and the transfer-function assumption.
///
/// Gain is single-channel: each pixel's base and alt luma (BT.709-weighted,
/// see [`LUMA`]) are combined into one ratio, matching libavif's YUV400
/// gain-map path. Cost: on a highlight that's clipped in one channel only
/// (e.g. a saturated red light), luma under-represents that channel's real
/// gain need, so reconstruction of that channel will be off by more than the
/// luma-channel error alone — a 3-channel gain map (ISO 21496-1 permits it)
/// would track this, at 3x the metadata/plane cost.
pub fn derive(hdr: &Rgb, sdr_base: &Rgb, opts: &DeriveOptions) -> (GainPlane, GainMapMeta) {
    assert_eq!(hdr.width, sdr_base.width, "hdr/base width mismatch");
    assert_eq!(hdr.height, sdr_base.height, "hdr/base height mismatch");
    let (w, h) = (hdr.width, hdr.height);
    let n = w as usize * h as usize;

    let mut log2_gain = vec![0f32; n];
    for y in 0..h {
        for x in 0..w {
            let base = luma_linear(sdr_base, x, y);
            let alt = luma_linear(hdr, x, y);
            let ratio = (alt + opts.alt_offset) / (base + opts.base_offset);
            log2_gain[y as usize * w as usize + x as usize] = ratio.max(EPS).log2();
        }
    }

    let mut min_log2 = f32::INFINITY;
    let mut max_log2 = f32::NEG_INFINITY;
    for &v in &log2_gain {
        min_log2 = min_log2.min(v);
        max_log2 = max_log2.max(v);
    }
    if !min_log2.is_finite() || !max_log2.is_finite() {
        min_log2 = 0.0;
        max_log2 = 0.0;
    }
    let range = (max_log2 - min_log2).max(0.0);

    let subsample = opts.subsample.max(1);
    let gw = w.div_ceil(subsample);
    let gh = h.div_ceil(subsample);
    let plane_len = gw as usize * gh as usize;
    let mut sum = vec![0f32; plane_len];
    let mut count = vec![0u32; plane_len];
    for y in 0..h {
        for x in 0..w {
            let v = log2_gain[y as usize * w as usize + x as usize];
            let norm = if range > 0.0 {
                ((v - min_log2) / range).clamp(0.0, 1.0).powf(opts.gamma)
            } else {
                0.0
            };
            let gi = (y / subsample) as usize * gw as usize + (x / subsample) as usize;
            sum[gi] += norm;
            count[gi] += 1;
        }
    }
    let data: Vec<u8> = sum
        .iter()
        .zip(count.iter())
        .map(|(&s, &c)| {
            let avg = if c > 0 { s / c as f32 } else { 0.0 };
            (avg.clamp(0.0, 1.0) * 255.0).round() as u8
        })
        .collect();

    let meta = GainMapMeta {
        min_log2: [min_log2; 3],
        max_log2: [max_log2; 3],
        gamma: [opts.gamma; 3],
        base_offset: [opts.base_offset; 3],
        alt_offset: [opts.alt_offset; 3],
        base_headroom: 0.0,
        alt_headroom: opts.alt_headroom,
        use_base_color_space: true,
    };
    (
        GainPlane {
            width: gw,
            height: gh,
            data,
        },
        meta,
    )
}

/// Reconstruct the HDR (alternate) image from a base image, a gain plane,
/// and reconstruction metadata. Inverse of [`derive`]; see module docs for
/// the equation. Upsamples the gain plane to `base` resolution with bilinear
/// filtering when its resolution differs (see [`sample_gain_bilinear`]).
pub fn apply(base: &Rgb, gain: &GainPlane, meta: &GainMapMeta) -> Rgb {
    let (w, h) = (base.width, base.height);
    let mut data = vec![0u16; base.data.len()];
    let max_val = base.max_value() as f32;

    // Single-channel plane: same min/max/gamma/offset used for all 3 channels.
    let min_log2 = meta.min_log2[0];
    let max_log2 = meta.max_log2[0];
    let range = max_log2 - min_log2;
    let gamma = meta.gamma[0].max(EPS);
    let base_offset = meta.base_offset[0];
    let alt_offset = meta.alt_offset[0];

    for y in 0..h {
        for x in 0..w {
            let g = sample_gain_bilinear(gain, x, y, w, h);
            let decoded_log2 = if range > 0.0 {
                min_log2 + g.powf(1.0 / gamma) * range
            } else {
                min_log2
            };
            let scale = decoded_log2.exp2();
            for c in 0..3 {
                let base_lin = srgb_to_linear(sample_encoded(base, x, y, c));
                let alt_lin = ((base_lin + base_offset) * scale - alt_offset).max(0.0);
                let alt_enc = linear_to_srgb(alt_lin.min(1.0));
                let idx = (y as usize * w as usize + x as usize) * 3 + c;
                data[idx] = (alt_enc * max_val).round() as u16;
            }
        }
    }

    Rgb {
        width: w,
        height: h,
        bits: base.bits,
        data,
    }
}
