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
mod input;

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

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

pub type Result<T> = core::result::Result<T, Error>;

/// The portable encoder.
#[derive(Debug, Default, Clone, Copy)]
pub struct PortableEngine;

impl GainMapEncoder for PortableEngine {
    type Error = Error;

    fn name(&self) -> &'static str {
        "portable-hpvca"
    }

    fn encode(
        &self,
        base: &Rgb,
        gain: &GainPlane,
        meta: &GainMapMeta,
        opts: &EncodeOptions,
    ) -> Result<Vec<u8>> {
        let base_heic = codec::encode_base_heic(base, opts.base_quality)
            .map_err(|e| Error::Encode(format!("base: {e}")))?;
        let gain_heic = codec::encode_gain_heic(gain, opts.gain_quality)
            .map_err(|e| Error::Encode(format!("gain: {e}")))?;

        // Both HEICs above are hpvca's own single-item container, not our
        // target multi-item gain-map file — pull the coded HEVC + `hvcC` back
        // out of each so `tohdr_heif::mux` can re-assemble them together.
        let base_file = tohdr_heif::HeifFile::parse(&base_heic)?;
        let base_item = base_file
            .primary_item()
            .ok_or_else(|| Error::Encode("hpvca base output has no primary item".into()))?;
        let base_coded = base_file.coded_image(base_item)?;

        let gain_file = tohdr_heif::HeifFile::parse(&gain_heic)?;
        let gain_item = gain_file
            .primary_item()
            .ok_or_else(|| Error::Encode("hpvca gain output has no primary item".into()))?;
        let gain_coded = gain_file.coded_image(gain_item)?;

        let req = tohdr_heif::MuxRequest {
            base: base_coded,
            gain: gain_coded,
            meta: *meta,
            flavor: opts.flavor,
            base_colour: Some(tohdr_heif::ColourInfo::Nclx {
                primaries: 1,  // BT.709 / sRGB
                transfer: 13,  // sRGB
                matrix: 6,     // BT.601
                full_range: true,
            }),
            // The `tmap` describes the reconstructed HDR image, not the SDR
            // base: Display P3 primaries with the PQ transfer, which is what
            // `IMG_4913.HEIC` puts here (as an ICC profile rather than
            // `nclx`).
            tmap_colour: Some(tohdr_heif::ColourInfo::Nclx {
                primaries: 12, // Display P3
                transfer: 16,  // SMPTE ST 2084 (PQ)
                matrix: 6,
                full_range: true,
            }),
            exif: None,
            // Apple writes the headroom three times and all three agree; a
            // consumer reading the XMP copy rather than the tmap must not get
            // a different number. Only emitted for flavors that claim Apple
            // compatibility, since it is Apple's namespace.
            xmp: opts
                .flavor
                .writes_apple()
                .then(|| tohdr_core::xmp::headroom_packet(meta.alt_headroom)),
            clli: None,
        };
        Ok(tohdr_heif::mux(&req)?)
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
