//! Engine B: portable HEVC encode plus our own muxer.
//!
//! No Apple frameworks, no GPL codec. `hpvca` (BSD-3/Apache) encodes the base
//! and the gain plane as two independent single-item HEICs;
//! [`tohdr_heif::mux`] then assembles them into one multi-item gain-map file.
//!
//! `spikes/hpvca-probe` established the substrate: hpvca output decodes through
//! Apple ImageIO at 8- and 10-bit, runs ~38 MP/s at 21.8 MP, and with
//! `ParallelismStrategy::TilesWpp` yields exactly one image item (no grid, no
//! `iref`) at the same speed as the gridded default and a smaller file — which
//! is what makes it remuxable without reassembling tiles.

use std::path::Path;

use tohdr_core::{EncodeOptions, GainMapEncoder, GainMapMeta, GainPlane, HdrRgb, Rgb};

mod codec;
pub mod exif;
pub mod gainmap_tiff;
mod input;

pub use codec::{HpvcaCodec, YUV444_QUALITY_THRESHOLD};
pub use exif::{read as read_source_exif, Origin as ExifOrigin, SourceExif};
pub use gainmap_tiff::{read as read_gainmap_tiff, GainMapTiff};
pub use input::{load_hdr_tiff_pq, DEFAULT_REFERENCE_WHITE_NITS};

#[derive(Debug)]
pub enum Error {
    Encode(String),
    Mux(tohdr_heif::Error),
    /// A source file's format is not one of the pure-Rust decoders we carry.
    UnsupportedInput(String),
    Decode(String),
    Io(std::io::Error),
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::Encode(m) => write!(f, "hevc encode: {m}"),
            Error::Mux(e) => write!(f, "mux: {e}"),
            Error::UnsupportedInput(m) => write!(f, "unsupported input: {m}"),
            Error::Decode(m) => write!(f, "decode: {m}"),
            Error::Io(e) => write!(f, "io: {e}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<tohdr_heif::Error> for Error {
    fn from(e: tohdr_heif::Error) -> Self {
        Error::Mux(e)
    }
}

impl From<tohdr_heif::MuxEngineError> for Error {
    fn from(e: tohdr_heif::MuxEngineError) -> Self {
        match e {
            tohdr_heif::MuxEngineError::Encode { plane, message } => {
                Error::Encode(format!("{plane}: {message}"))
            }
            tohdr_heif::MuxEngineError::Mux(e) => Error::Mux(e),
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

pub type Result<T> = core::result::Result<T, Error>;

/// The portable encoder: [`HpvcaCodec`] wired into [`tohdr_heif::MuxEngine`].
///
/// A named type rather than a type alias so the many call sites that use it as a
/// unit value keep working, and so this crate's error type stays the one they
/// already match on.
#[derive(Debug, Default, Clone, Copy)]
pub struct PortableEngine;

impl GainMapEncoder for PortableEngine {
    type Error = Error;

    fn name(&self) -> &'static str {
        "portable-hpvca"
    }

    fn carries_exif(&self) -> bool {
        tohdr_heif::MuxEngine::new(HpvcaCodec).carries_exif()
    }

    fn encode(
        &self,
        base: &Rgb,
        gain: &GainPlane,
        meta: &GainMapMeta,
        opts: &EncodeOptions,
    ) -> Result<Vec<u8>> {
        Ok(tohdr_heif::MuxEngine::new(HpvcaCodec).encode(base, gain, meta, opts)?)
    }
}

/// Decode an HDR source with pure-Rust decoders only (no Apple frameworks), into
/// linear extended-range RGB with `1.0` at SDR diffuse white.
///
/// See [`input`] module docs for the colour-space assumption each supported
/// format gets (16-bit TIFF, PNG, JPEG) and how to override it.
pub fn load_hdr(path: &Path) -> Result<HdrRgb> {
    input::load_hdr(path)
}

/// Decode an SDR source with pure-Rust decoders only, assumed sRGB-encoded.
pub fn load_sdr(path: &Path) -> Result<Rgb> {
    input::load_sdr(path)
}

/// Escape hatch for `examples/dump_gain_intermediate.rs`: hpvca's own
/// single-item HEIC for the gain plane, before the muxer re-wraps it. Exists
/// so the encoder's `pixi`/`hvcC` can be inspected separately from ours.
#[doc(hidden)]
pub fn debug_encode_gain_heic(gain: &GainPlane, quality: u8) -> Result<Vec<u8>> {
    codec::encode_gain_heic(gain, quality).map_err(|e| Error::Encode(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scene_rgb8(w: u32, h: u32) -> Vec<u16> {
        let mut v = Vec::with_capacity((w * h * 3) as usize);
        for y in 0..h {
            for x in 0..w {
                let fx = x as f32 / w as f32;
                let fy = y as f32 / h as f32;
                v.push((fx * 255.0) as u16);
                v.push((fy * 255.0) as u16);
                v.push(128);
            }
        }
        v
    }

    /// End-to-end: base + gain -> two hpvca HEICs -> tohdr_heif parse/mux.
    #[test]
    fn full_pipeline_mux() {
        let (w, h) = (64, 48);
        let base = Rgb {
            width: w,
            height: h,
            bits: 8,
            data: scene_rgb8(w, h),
        };
        let gain = GainPlane {
            width: w / 2,
            height: h / 2,
            data: vec![128u8; (w / 2 * h / 2) as usize],
        };
        let meta = GainMapMeta::default();
        let opts = EncodeOptions::default();
        let engine = PortableEngine;
        let bytes = engine.encode(&base, &gain, &meta, &opts).expect("mux");
        assert!(!bytes.is_empty());
    }
}
