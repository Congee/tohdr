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
        let _ = (base, gain, meta, opts);
        todo!("hpvca encode + tohdr-heif mux")
    }
}

/// Decode an HDR source with pure-Rust decoders only (no Apple frameworks), into
/// linear extended-range RGB with `1.0` at SDR diffuse white.
pub fn load_hdr(path: &Path) -> Result<HdrRgb> {
    let _ = path;
    todo!("portable HDR decode")
}

/// Decode an SDR source with pure-Rust decoders only.
pub fn load_sdr(path: &Path) -> Result<Rgb> {
    let _ = path;
    todo!("portable SDR decode")
}
