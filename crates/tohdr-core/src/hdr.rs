//! Extended-range HDR pixels, tone mapping to an SDR base, and the
//! headroom-consistent derivation that [`crate::derive`] alone cannot express.
//!
//! [`crate::Rgb`] encodes `0..=max_value`, so above-white light is
//! unrepresentable. Since carrying that light is a gain map's whole purpose, the
//! encode pipeline starts from [`HdrRgb`]: linear `f32`, `1.0` at diffuse white,
//! unbounded above.
//!
//! Derivation lives here rather than in [`crate::derive`] because that module
//! takes `alt_headroom` verbatim from its options while computing
//! `min_log2`/`max_log2` from pixels -- so a caller can declare more headroom than
//! the plane encodes. A conformant renderer weights the map by
//! `(display - base) / (alt - base)`, so over-declaring makes it *under-apply* and
//! the flat base shows through. [`derive_consistent`] derives the declared
//! headroom from the plane itself, the invariant the reference capture holds and both
//! washed-out exports break.

use crate::derive::{self, sample_gain_bilinear, DeriveOptions, EPS, LUMA};
use crate::par;
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
        // Per-chunk histograms merged afterwards: a histogram is a reduction
        // with a tiny (4 KiB) accumulator, so each worker can own a private
        // one and there is no contention at all.
        let row_len = self.width as usize * 3;
        let partials = par::map_row_chunks(&self.data, row_len, 1, |_, chunk| {
            let mut hist = [0u32; BUCKETS];
            for px in chunk.chunks_exact(3) {
                let l = (LUMA[0] * px[0] + LUMA[1] * px[1] + LUMA[2] * px[2])
                    .max(0.0)
                    .max(EPS);
                let stops = l.log2().clamp(LO_STOPS, HI_STOPS);
                let b = ((stops - LO_STOPS) / (HI_STOPS - LO_STOPS)
                    * (BUCKETS - 1) as f32) as usize;
                hist[b.min(BUCKETS - 1)] += 1;
            }
            hist
        });
        let mut hist = vec![0u32; BUCKETS];
        for p in &partials {
            for (h, v) in hist.iter_mut().zip(p.iter()) {
                *h += *v;
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
        let row_len = hdr.width as usize * 3;
        // Output rows map one-to-one onto input rows, so both slices can be
        // chunked the same way and each worker owns a disjoint span.
        let src = &hdr.data;
        let tone = *self;
        par::for_each_row_chunk_mut(&mut data, row_len, 1, |start_row, out| {
            let base = start_row * row_len;
            for (i, o) in out.chunks_exact_mut(3).enumerate() {
                let s = base + i * 3;
                let (r, g, b) = (src[s], src[s + 1], src[s + 2]);
                let l = (LUMA[0] * r + LUMA[1] * g + LUMA[2] * b).max(0.0);
                // At l == 0 there is no chroma to preserve, so the ratio is
                // irrelevant; 1.0 keeps the (already black) pixel black.
                let scale = if l > EPS { tone.map_luma(l) / l } else { 1.0 };
                o[0] = derive::linear_to_srgb8(r * scale) as u16;
                o[1] = derive::linear_to_srgb8(g * scale) as u16;
                o[2] = derive::linear_to_srgb8(b * scale) as u16;
            }
        });
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
            derive::luma_linear(sdr_base, x, y)
        },
        opts,
    );
    // The plane delivers at most max_log2 stops, floored at zero because the ISO
    // headroom fields are *unsigned* (only min/max_log2 and the offsets are
    // signed -- ISO 21496-1 C.2.2, and libavif's types agree). Without the floor a
    // negative headroom serialised as 0 while `max_log2` stayed negative, breaking
    // the `max_log2 == alt_headroom` invariant and switching the map off.
    //
    // The floor is not a lossy compromise: a darkening map delivers no gain under
    // libavif's rules anyway, since the sign-flip branch clamps its weight to 0
    // for every display headroom. `min_log2` is left alone -- darkening some pixels
    // is legal; it is only the headroom that cannot go below zero.
    meta.alt_headroom = meta.max_log2[0].max(0.0);
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
                let base_lin = derive::sample_linear(base, x, y, c);
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
/// Port of libavif's `avifGetGainMapWeight`: interpolate in log2 stops between the
/// base and alternate headrooms, clamp to `0..=1`, and only *after* the clamp flip
/// sign when the alternate is darker. `0.0` when the two headrooms are equal, which
/// libavif leaves unspecified.
///
/// Clamp-then-flip is why the range is `[-1, 1]`: a negative result needs
/// `alt < base` *and* a display below the base headroom.
///
/// This is what punishes an over-declared `alt_headroom` -- raising it while
/// `max_log2` stays put shrinks the weight, so the flat SDR base shows through.
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
