//! hpvca configuration and pixel packing for [`crate::PortableEngine`].
//!
//! Both the base and the gain plane are encoded with
//! `ParallelismStrategy::TilesWpp`: per `spikes/hpvca-probe`, it is the
//! strategy that saturates cores exactly like the gridded default while still
//! producing exactly one coded image item (no HEIF `grid`, no `iref`).
//! [`tohdr_heif::HeifFile::coded_image`] explicitly refuses a `grid` item,
//! because reassembling tiles into one coded image is a re-encode, not a
//! remux — so any strategy that grids here would make the whole engine
//! unusable, not merely slower to remux.
//!
//! # The `encode_gray` pitfall
//!
//! hpvca's `encode_gray` (and friends) forces a HEIF grid for any plane wider
//! or taller than 512px **regardless of `ParallelismStrategy`**:
//! `encode_gray_wide` tiles unconditionally, unlike `encode_rgb_wide`, which
//! only grids when the strategy actually asks for one
//! (`needs_tiling(..) && cfg.parallelism.uses_grid()`). A full-resolution gain
//! plane on a real photo is almost always >512px on one side, so calling
//! `encode_gray` here would silently defeat the single-item requirement
//! above. We route the gain plane through `encode_yuv` with a hand-built
//! monochrome [`hpvca::Yuv`] instead — that path *does* respect
//! `cfg.parallelism`, the same as the RGB path.

use hpvca::{BitDepth, ChromaFormat, EncodeConfig, EncodeError, ParallelismStrategy, Yuv};
use tohdr_core::{GainPlane, Rgb};
use tohdr_heif::{CodedImage, PlaneCodec};

/// Above this `base_quality`, encode 4:4:4 instead of 4:2:0.
///
/// 4:4:4 avoids chroma-subsampling loss but roughly doubles chroma bits, so it
/// only pays for itself once quality is high enough that those bits would
/// otherwise go mostly unused by heavy quantization anyway. Tunable; not
/// derived from measurement.
///
/// Public because it is also a *codec selection* input: the hardware path is
/// 4:2:0 only, so asking for this quality means asking for this codec.
pub const YUV444_QUALITY_THRESHOLD: u8 = 95;

/// Picks 4:4:4 over 4:2:0 for the base image once quality is high enough that
/// chroma subsampling would be the visible bottleneck.
pub(crate) fn choose_base_chroma(base_quality: u8) -> ChromaFormat {
    if base_quality >= YUV444_QUALITY_THRESHOLD {
        ChromaFormat::Yuv444
    } else {
        ChromaFormat::Yuv420
    }
}

/// Shared config: quality plus the [`ParallelismStrategy::TilesWpp`]
/// requirement from the module docs. `chroma` is only meaningful for the RGB
/// path; the gray path below ignores whatever is set here (see
/// [`encode_gain_heic`]).
///
/// # Why SAO is off
///
/// hpvca implements Sample Adaptive Offset with an analysis encode *before* the
/// real one, so leaving it on roughly doubles the transform/RDO work — its own
/// docs say as much. It is on by default, and it was the single largest cost in
/// this engine. `examples/probe_engine_b_speed.rs` at 60 MP, q85:
///
/// ```text
///                          base ms   gain ms      bytes
///   sao on (the default)     1714.1     970.0     215148
///   sao off                   643.3     431.1     202559
/// ```
///
/// 2.5x faster *and* 6% smaller — SAO was not paying for its bits here, so this
/// is not the usual speed-for-quality trade. The same holds on a real 60 MP
/// photograph, where it is what takes this engine from 15x Engine A to
/// competitive; see `docs/engine-comparison.md`. Variance boost was measured at
/// the same time and left alone: it costs ~25% of the base encode with SAO on,
/// nothing with SAO off, and changes no bytes either way.
pub(crate) fn config_for(quality: u8, chroma: ChromaFormat) -> EncodeConfig {
    EncodeConfig::default()
        .with_quality(quality.clamp(1, 100))
        .with_parallelism(ParallelismStrategy::TilesWpp)
        .with_chroma(chroma)
        .with_sao(false)
}

/// Encode the SDR base as a single-item HEIC.
///
/// Dispatches on `base.bits`; hpvca supports exactly 8/10/12-bit RGB, matching
/// [`Rgb::max_value`]'s three legal bit depths. `opts.base_quality` maps
/// directly onto hpvca's `quality` (both `1..=100`), so there is no
/// non-trivial remapping to test beyond the chroma threshold above.
pub(crate) fn encode_base_heic(base: &Rgb, quality: u8) -> Result<Vec<u8>, EncodeError> {
    let chroma = choose_base_chroma(quality);
    let cfg = config_for(quality, chroma);
    match base.bits {
        8 => {
            // hpvca's 8-bit entry point wants tightly packed `u8`; `Rgb`
            // stores `u16` uniformly across depths, so narrow here. Safe
            // because `Rgb::max_value()` for bits==8 is 255.
            let packed: Vec<u8> = base.data.iter().map(|&v| v as u8).collect();
            hpvca::encode_rgb(&packed, base.width, base.height, &cfg)
        }
        10 => hpvca::encode_rgb10(&base.data, base.width, base.height, &cfg),
        12 => hpvca::encode_rgb12(&base.data, base.width, base.height, &cfg),
        // hpvca has no such depth; InvalidInput is the closest existing
        // variant (see `hpvca::fmt::BitDepth::from_bits`, which panics on the
        // same condition rather than erroring — we'd rather not panic here).
        _ => Err(EncodeError::InvalidInput),
    }
}

/// Encode the gain plane as monochrome 8-bit, per ISO 21496-1/Apple's
/// convention that a gain map is single-channel. Routed through `encode_yuv`
/// rather than `encode_gray`; see the module docs for why.
pub(crate) fn encode_gain_heic(gain: &GainPlane, quality: u8) -> Result<Vec<u8>, EncodeError> {
    let y: Vec<u16> = gain.data.iter().map(|&v| v as u16).collect();
    let yuv = Yuv::from_planes(
        y,
        Vec::new(),
        Vec::new(),
        gain.width,
        gain.height,
        ChromaFormat::Monochrome,
        BitDepth::Eight,
    )?;
    // `chroma` in the config is irrelevant here — `encode_yuv`'s docs say
    // `cfg.chroma` is ignored in favor of what the `Yuv` itself carries — but
    // `Monochrome` documents intent at the call site.
    let cfg = config_for(quality, ChromaFormat::Monochrome);
    hpvca::encode_yuv(&yuv, &cfg)
}

/// The pure-Rust plane codec: no Apple frameworks, no GPL, no media block.
///
/// Slower than the platform encoder by roughly 6x on Apple Silicon (see
/// `docs/engine-comparison.md`), and the only backend available where there is
/// no hardware path — so it stays the fallback rather than being retired.
#[derive(Debug, Default, Clone, Copy)]
pub struct HpvcaCodec;

impl PlaneCodec for HpvcaCodec {
    type Error = crate::Error;

    fn name(&self) -> &'static str {
        "portable-hpvca"
    }

    /// hpvca converts RGB with the **BT.601** matrix. Measured, not read off a
    /// header: declaring BT.601 reconstructs at 69.31 dB and declaring BT.709 at
    /// 51.81 dB on the same coded bytes (`probe_vt_colour.rs`). The media block
    /// picks the other one, which is why this lives on the codec.
    fn base_colour(&self, primaries: tohdr_core::Primaries) -> tohdr_heif::ColourInfo {
        tohdr_heif::ColourInfo::Nclx {
            // The loader's decision, not this codec's: hpvca codes whatever RGB
            // it is handed and cannot tell one set of primaries from another.
            primaries: primaries.nclx(),
            transfer: 13, // sRGB
            matrix: 6,    // BT.601 / SMPTE 170M
            // ImageIO's reconstruction is indifferent to this flag on both
            // codecs (same PSNR either way), so it stays as it has been.
            full_range: true,
        }
    }

    fn encode_base(&self, base: &Rgb, quality: u8) -> Result<CodedImage, crate::Error> {
        // hpvca hands back its own single-item HEIC rather than a bare
        // bitstream, so the coded HEVC has to come back out before it can be
        // placed next to the gain plane in one file.
        let heic =
            encode_base_heic(base, quality).map_err(|e| crate::Error::Encode(e.to_string()))?;
        Ok(tohdr_heif::coded_image_from_heic(&heic, "base")?)
    }

    fn encode_gain(&self, gain: &GainPlane, quality: u8) -> Result<CodedImage, crate::Error> {
        let heic =
            encode_gain_heic(gain, quality).map_err(|e| crate::Error::Encode(e.to_string()))?;
        Ok(tohdr_heif::coded_image_from_heic(&heic, "gain")?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Same synthetic-scene shape as `spikes/hpvca-probe`: a diagonal
    /// gradient plus a hot corner, so there is real structure to compress.
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

    /// First box in any well-formed ISOBMFF/HEIF file is `ftyp`, size-prefixed.
    fn looks_like_isobmff(bytes: &[u8]) -> bool {
        bytes.len() > 12 && &bytes[4..8] == b"ftyp"
    }

    /// Locks in the 21 dB finding from `probe_vt_colour.rs`: this codec's output
    /// must be declared BT.601. The hardware codec declares BT.709, and the day
    /// these two agree by accident is the day one of them is wrong.
    #[test]
    fn declares_the_bt601_matrix_it_actually_writes() {
        for p in tohdr_core::Primaries::ALL {
            match PlaneCodec::base_colour(&HpvcaCodec, p) {
                tohdr_heif::ColourInfo::Nclx { matrix, primaries, transfer, .. } => {
                    assert_eq!(matrix, 6, "hpvca writes BT.601; see probe_vt_colour.rs");
                    assert_eq!(transfer, 13);
                    // The matrix is the codec's fact and must not move with the
                    // primaries; the primaries are the caller's and must.
                    assert_eq!(primaries, p.nclx(), "{p:?} was not passed through");
                }
                other => panic!("expected an nclx declaration, got {other:?}"),
            }
        }
    }

    #[test]
    fn chroma_threshold() {
        assert_eq!(choose_base_chroma(80), ChromaFormat::Yuv420);
        assert_eq!(choose_base_chroma(94), ChromaFormat::Yuv420);
        assert_eq!(choose_base_chroma(95), ChromaFormat::Yuv444);
        assert_eq!(choose_base_chroma(100), ChromaFormat::Yuv444);
    }

    #[test]
    fn base_8bit_encodes_to_a_real_heic() {
        let (w, h) = (64, 48);
        let base = Rgb {
            width: w,
            height: h,
            bits: 8,
            data: scene_rgb8(w, h).into_iter().map(|b| b as u16).collect(),
        };
        let bytes = encode_base_heic(&base, 80).expect("encode");
        assert!(looks_like_isobmff(&bytes));
        assert!(!bytes.is_empty());
    }

    #[test]
    fn base_10bit_encodes_to_a_real_heic() {
        let (w, h) = (64, 48);
        let data: Vec<u16> = scene_rgb8(w, h).into_iter().map(|b| (b as u16) << 2).collect();
        let base = Rgb {
            width: w,
            height: h,
            bits: 10,
            data,
        };
        let bytes = encode_base_heic(&base, 90).expect("encode");
        assert!(looks_like_isobmff(&bytes));
    }

    #[test]
    fn base_12bit_encodes_to_a_real_heic() {
        let (w, h) = (64, 48);
        let data: Vec<u16> = scene_rgb8(w, h).into_iter().map(|b| (b as u16) << 4).collect();
        let base = Rgb {
            width: w,
            height: h,
            bits: 12,
            data,
        };
        let bytes = encode_base_heic(&base, 90).expect("encode");
        assert!(looks_like_isobmff(&bytes));
    }

    #[test]
    fn unsupported_base_bit_depth_errors_without_panicking() {
        let base = Rgb {
            width: 4,
            height: 4,
            bits: 16,
            data: vec![0u16; 4 * 4 * 3],
        };
        assert!(encode_base_heic(&base, 80).is_err());
    }

    #[test]
    fn gain_plane_small_encodes_to_a_real_heic() {
        let (w, h) = (32, 24);
        let gain = GainPlane {
            width: w,
            height: h,
            data: scene_gray8(w, h),
        };
        let bytes = encode_gain_heic(&gain, 80).expect("encode");
        assert!(looks_like_isobmff(&bytes));
    }

    /// The regression this module exists to prevent: a gain plane larger than
    /// hpvca's 512px tile threshold must still come out as a single item, not
    /// a `grid`. We can't parse items without `tohdr_heif` yet (still
    /// `todo!()`), so this only checks for the literal `grid` fourcc as a
    /// coarse signal; the authoritative check is
    /// `heif_detail.py` against a real encoded file (see engine report).
    #[test]
    fn gain_plane_over_tile_threshold_has_no_grid_fourcc() {
        let (w, h) = (600, 400); // > hpvca's 512px TILE_SIZE on both axes
        let gain = GainPlane {
            width: w,
            height: h,
            data: scene_gray8(w, h),
        };
        let bytes = encode_gain_heic(&gain, 80).expect("encode");
        assert!(looks_like_isobmff(&bytes));
        assert!(
            !bytes.windows(4).any(|w| w == b"grid"),
            "gain plane over the tile threshold produced a HEIF grid"
        );
    }

    #[test]
    fn higher_quality_does_not_shrink_output() {
        let (w, h) = (64, 48);
        let base = Rgb {
            width: w,
            height: h,
            bits: 8,
            data: scene_rgb8(w, h).into_iter().map(|b| b as u16).collect(),
        };
        let low = encode_base_heic(&base, 40).expect("encode");
        let high = encode_base_heic(&base, 90).expect("encode");
        assert!(high.len() >= low.len());
    }
}
