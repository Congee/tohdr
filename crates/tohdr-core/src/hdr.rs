//! Extended-range HDR pixels, tone mapping to an SDR base, and the
//! headroom-consistent derivation that [`crate::derive`] alone cannot express.
//!
//! [`crate::Rgb`] is fixed-range: its samples encode `0..=max_value`, so linear
//! light above SDR diffuse white is unrepresentable ([`crate::derive`] module
//! docs, "Transfer function"). A gain map's entire purpose is to carry that
//! above-white light, so the encode pipeline starts from [`HdrRgb`] instead:
//! linear, `f32`, `1.0` at SDR diffuse white, unbounded above.
//!
//! # Why derivation lives here and not in [`crate::derive`]
//!
//! [`crate::derive`] takes `alt_headroom` verbatim from its options and computes
//! `min_log2`/`max_log2` from pixel data, so nothing stops a caller from
//! declaring more headroom than the plane encodes. That mismatch is exactly the
//! defect diagnosed in `docs/heic-gainmap-structure.md`: a conformant renderer
//! weights the map by `(display - base) / (alt - base)` (libavif
//! `src/gainmap.c:61`), so over-declaring makes it *under-apply* the map and the
//! flat SDR base shows through. [`derive_consistent`] closes that hole by
//! deriving the declared headroom from the plane itself, the invariant
//! `IMG_4913.HEIC` holds and both washed-out exports break.

use crate::derive::{
    self, sample_gain_bilinear, srgb_to_linear, DeriveOptions, EPS, LUMA,
};
use crate::{GainMapMeta, GainPlane, Rgb};

/// Interleaved linear-light RGB, `f32` per sample, `1.0` at SDR diffuse white.
///
/// Values above `1.0` are the HDR headroom and are expected, not an error.
/// Negative values are out-of-gamut and are clamped at use sites rather than
/// rejected here, so a wide-gamut source converted into a narrower working
/// space still encodes.
#[derive(Clone, Debug)]
pub struct HdrRgb {
    pub width: u32,
    pub height: u32,
    /// `width * height * 3`, interleaved RGB.
    pub data: Vec<f32>,
}

impl HdrRgb {
    pub fn expected_len(&self) -> usize {
        self.width as usize * self.height as usize * 3
    }

    pub fn black(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            data: vec![0.0; width as usize * height as usize * 3],
        }
    }

    #[inline]
    pub fn pixel(&self, x: u32, y: u32) -> [f32; 3] {
        let i = (y as usize * self.width as usize + x as usize) * 3;
        [self.data[i], self.data[i + 1], self.data[i + 2]]
    }

    #[inline]
    pub fn luma(&self, x: u32, y: u32) -> f32 {
        let [r, g, b] = self.pixel(x, y);
        (LUMA[0] * r + LUMA[1] * g + LUMA[2] * b).max(0.0)
    }

    /// Linear-light peak, ignoring the brightest `outlier_fraction` of pixels.
    ///
    /// A single specular pixel or a hot-pixel artifact would otherwise set the
    /// whole map's range and crush every real highlight into the bottom of the
    /// 8-bit plane. libavif trims the same way before picking min/max
    /// (`avifFindMinMaxWithoutOutliers`, `src/gainmap.c:375-429`), which the
    /// raw-min/max [`crate::derive`] deliberately skips.
    pub fn peak_luma(&self, outlier_fraction: f64) -> f32 {
        let n = self.width as usize * self.height as usize;
        if n == 0 {
            return 1.0;
        }
        // 1024 log2-spaced buckets over 1/256x..256x SDR white. Coarse on
        // purpose: this picks a headroom to declare, and a histogram is O(n)
        // where a full sort is O(n log n) on tens of megapixels.
        const BUCKETS: usize = 1024;
        const LO_STOPS: f32 = -8.0;
        const HI_STOPS: f32 = 8.0;
        let mut hist = vec![0u32; BUCKETS];
        for y in 0..self.height {
            for x in 0..self.width {
                let l = self.luma(x, y).max(EPS);
                let stops = l.log2().clamp(LO_STOPS, HI_STOPS);
                let b = ((stops - LO_STOPS) / (HI_STOPS - LO_STOPS) * (BUCKETS - 1) as f32) as usize;
                hist[b.min(BUCKETS - 1)] += 1;
            }
        }
        let budget = (n as f64 * outlier_fraction.clamp(0.0, 1.0)) as u64;
        let mut dropped = 0u64;
        for b in (0..BUCKETS).rev() {
            dropped += hist[b] as u64;
            if dropped > budget {
                let stops =
                    LO_STOPS + (b as f32 / (BUCKETS - 1) as f32) * (HI_STOPS - LO_STOPS);
                return stops.exp2().max(1.0);
            }
        }
        1.0
    }
}

/// How to render the SDR base that ships alongside the gain map.
///
/// The base is what non-HDR viewers see, so it must stand alone as a good SDR
/// photo; the gain map only has to explain the difference. Both operators are
/// applied to luma with chroma held as a ratio, so hue does not shift as
/// highlights compress.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ToneMap {
    /// Hard-clip above SDR white. Preserves the SDR look exactly up to the
    /// clip point and throws away everything above it — the gain map then
    /// carries a large, abrupt range. Cheap and faithful for images whose
    /// highlights are mostly specular.
    Clip,
    /// Extended Reinhard, `Ld = L (1 + L/white²) / (1 + L)`, which maps
    /// `L = white` exactly to `1.0` and is monotonic below it. Rolls highlights
    /// off smoothly, so the gain map carries a smaller range and quantizes
    /// better, at the cost of a slightly flatter SDR rendition.
    Reinhard {
        /// Linear luma that maps to SDR white. Typically [`HdrRgb::peak_luma`].
        white: f32,
    },
}

impl ToneMap {
    /// Map linear HDR luma to linear SDR luma in `0.0..=1.0`.
    #[inline]
    fn map_luma(&self, l: f32) -> f32 {
        match *self {
            ToneMap::Clip => l.clamp(0.0, 1.0),
            ToneMap::Reinhard { white } => {
                let w = white.max(1.0);
                (l * (1.0 + l / (w * w)) / (1.0 + l)).clamp(0.0, 1.0)
            }
        }
    }

    /// Render an 8-bit sRGB-encoded SDR base.
    ///
    /// Chroma is preserved by scaling all three channels by the same
    /// `mapped_luma / luma` ratio, then clamping; a per-channel curve would
    /// desaturate bright colors toward white.
    pub fn to_sdr(&self, hdr: &HdrRgb) -> Rgb {
        assert_eq!(hdr.data.len(), hdr.expected_len(), "HdrRgb length mismatch");
        let mut data = vec![0u16; hdr.expected_len()];
        for y in 0..hdr.height {
            for x in 0..hdr.width {
                let [r, g, b] = hdr.pixel(x, y);
                let l = hdr.luma(x, y);
                // At l == 0 there is no chroma to preserve, so the ratio is
                // irrelevant; 1.0 keeps the (already black) pixel black.
                let scale = if l > EPS { self.map_luma(l) / l } else { 1.0 };
                let i = (y as usize * hdr.width as usize + x as usize) * 3;
                for (c, v) in [r, g, b].iter().enumerate() {
                    let enc = derive::linear_to_srgb((v * scale).clamp(0.0, 1.0));
                    data[i + c] = (enc * 255.0).round() as u16;
                }
            }
        }
        Rgb {
            width: hdr.width,
            height: hdr.height,
            bits: 8,
            data,
        }
    }
}

/// Derive a gain plane whose declared headroom matches what it actually encodes.
///
/// Equivalent to [`crate::derive::derive`] except that `alt_headroom` is *not*
/// taken from `opts`: it is set to the derived `max_log2`, so
/// `max_log2 == alt_headroom` always holds. See the module docs for why that
/// invariant is the difference between an HDR render and a washed-out one.
///
/// `opts.alt_headroom` is ignored; everything else (subsample, offsets, gamma)
/// is honored.
pub fn derive_consistent(
    hdr: &HdrRgb,
    sdr_base: &Rgb,
    opts: &DeriveOptions,
) -> (GainPlane, GainMapMeta) {
    assert_eq!(hdr.width, sdr_base.width, "hdr/base width mismatch");
    assert_eq!(hdr.height, sdr_base.height, "hdr/base height mismatch");
    let (plane, mut meta) = derive::derive_from_luma(
        hdr.width,
        hdr.height,
        |x, y| hdr.luma(x, y),
        |x, y| {
            let r = srgb_to_linear(derive::sample_encoded(sdr_base, x, y, 0));
            let g = srgb_to_linear(derive::sample_encoded(sdr_base, x, y, 1));
            let b = srgb_to_linear(derive::sample_encoded(sdr_base, x, y, 2));
            LUMA[0] * r + LUMA[1] * g + LUMA[2] * b
        },
        opts,
    );
    // The plane can deliver at most max_log2 stops, so that is the honest
    // declaration — unconditionally, including when it is negative.
    //
    // Clamping this to `max(0.0)` looked harmless but broke the one invariant
    // this function exists to hold. A base brighter than the source (an
    // independently graded SDR trim with lifted shadows, say) yields a
    // negative max_log2; clamping left `alt_headroom = 0` while `max_log2`
    // stayed negative, so the two disagreed. Worse, with `base_headroom` also
    // 0 that made `base == alt`, and `gain_weight` returns 0 for *every*
    // display in that case — silently switching the map off rather than
    // applying a small one.
    //
    // A negative `alt_headroom` is representable (the ISO field is signed) and
    // well defined: `gain_weight` takes libavif's `alt < base` branch, which
    // correctly gives a positive-headroom display no gain from a darkening map.
    meta.alt_headroom = meta.max_log2[0];
    (plane, meta)
}

/// Reconstruct extended-range HDR from a base image, a gain plane, and metadata.
///
/// Unlike [`crate::derive::apply`], which re-encodes into [`Rgb`] and therefore
/// clamps at linear `1.0`, this keeps the above-white result — without it the
/// round-trip error of any real HDR source is unmeasurable, because every
/// highlight lands on the clamp.
///
/// Always fully applies the map (weight `1.0`), i.e. it models a display with at
/// least `meta.alt_headroom` stops. Partial application is
/// [`gain_weight`].
pub fn apply_hdr(base: &Rgb, gain: &GainPlane, meta: &GainMapMeta) -> HdrRgb {
    let (w, h) = (base.width, base.height);
    let mut data = vec![0f32; w as usize * h as usize * 3];

    let min_log2 = meta.min_log2[0];
    let range = meta.max_log2[0] - min_log2;
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
            let i = (y as usize * w as usize + x as usize) * 3;
            for c in 0..3 {
                let base_lin = srgb_to_linear(derive::sample_encoded(base, x, y, c));
                data[i + c] = ((base_lin + base_offset) * scale - alt_offset).max(0.0);
            }
        }
    }

    HdrRgb {
        width: w,
        height: h,
        data,
    }
}

/// How much of the gain map a display with `display_headroom_stops` applies.
///
/// Port of libavif's `avifGetGainMapWeight` (`src/gainmap.c:52-63`): linear
/// interpolation in log2 stops between the base and alternate headrooms, then
/// `AVIF_CLAMP(.., 0, 1)`, and only *after* the clamp a sign flip when the
/// alternate is darker than the base. Returns `0.0` when the two headrooms are
/// equal — libavif calls that case unspecified and declines to apply the map.
///
/// Order matters: clamping first means a negative result is reachable only when
/// `alt < base` *and* the display sits below the base headroom (both numerator
/// and denominator negative, so the ratio is positive before the flip).
/// Returning `-w` there is why the range is `[-1, 1]`, not `[0, 1]`.
///
/// This is the function that punishes an over-declared `alt_headroom`: raising
/// it while the plane's `max_log2` stays put shrinks the weight, so less of the
/// map is applied and the flat SDR base shows through.
pub fn gain_weight(meta: &GainMapMeta, display_headroom_stops: f32) -> f32 {
    let base = meta.base_headroom;
    let alt = meta.alt_headroom;
    // libavif compares the two f32s for exact equality (`src/gainmap.c:56`);
    // an epsilon window here would diverge from it on near-equal headrooms.
    if base == alt {
        return 0.0;
    }
    let w = ((display_headroom_stops - base) / (alt - base)).clamp(0.0, 1.0);
    if alt < base { -w } else { w }
}
