//! Engine A: Apple ImageIO.
//!
//! Links CoreGraphics/ImageIO directly, so the output is produced by the same
//! code path Apple's own Photos export uses. That makes this engine both a
//! shipping backend and the **correctness oracle** for Engine B: whatever
//! `CGImageSource` reports about a file is, by definition, what the Apple
//! ecosystem will act on.
//!
//! Proven working by `spikes/imageio-probe`: reading
//! `kCGImageAuxiliaryDataTypeHDRGainMap` / `...ISOGainMap`, and writing with
//! `kCGImageDestinationEncodeToISOGainmap`.

use std::path::Path;

use tohdr_core::{EncodeOptions, GainMapEncoder, GainMapMeta, GainPlane, HdrRgb, Rgb};

#[derive(Debug)]
pub enum Error {
    /// A CoreFoundation constructor returned NULL.
    NullFromFramework(&'static str),
    /// `CGImageDestinationFinalize` reported failure.
    FinalizeFailed,
    /// The file could not be read, or ImageIO does not recognize its type.
    Unreadable(String),
    /// ImageIO opened the file but it lacks something we require.
    Missing(&'static str),
    Io(std::io::Error),
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::NullFromFramework(w) => write!(f, "ImageIO returned NULL from {w}"),
            Error::FinalizeFailed => write!(f, "CGImageDestinationFinalize failed"),
            Error::Unreadable(m) => write!(f, "unreadable: {m}"),
            Error::Missing(m) => write!(f, "missing: {m}"),
            Error::Io(e) => write!(f, "io: {e}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

pub type Result<T> = core::result::Result<T, Error>;

/// The ImageIO-backed encoder.
#[derive(Debug, Default, Clone, Copy)]
pub struct AppleEngine;

impl GainMapEncoder for AppleEngine {
    type Error = Error;

    fn name(&self) -> &'static str {
        "apple-imageio"
    }

    fn encode(
        &self,
        base: &Rgb,
        gain: &GainPlane,
        meta: &GainMapMeta,
        opts: &EncodeOptions,
    ) -> Result<Vec<u8>> {
        let _ = (base, gain, meta, opts);
        todo!("ImageIO gain-map write")
    }
}

/// Decode any ImageIO-supported file into linear extended-range HDR.
///
/// Handles the two input shapes that matter: a plain HDR file (16-bit TIFF, EXR,
/// HDR-tagged PNG/JPEG) and an existing gain-map HEIC, where the HDR image is
/// the *reconstruction* of base plus gain map rather than anything stored
/// directly. `1.0` in the result is SDR diffuse white.
pub fn load_hdr(path: &Path) -> Result<HdrRgb> {
    let _ = path;
    todo!("ImageIO HDR decode")
}

/// Decode the SDR base of a file, without applying any gain map.
pub fn load_sdr(path: &Path) -> Result<Rgb> {
    let _ = path;
    todo!("ImageIO SDR decode")
}

/// What macOS ImageIO reports about a file. The comparison target for Engine B
/// and the basis of `tohdr verify`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ReadBack {
    pub width: u32,
    pub height: u32,
    pub depth: u32,
    /// `kCGImageAuxiliaryDataTypeHDRGainMap` present (Apple flavor).
    pub apple_aux: bool,
    /// `kCGImageAuxiliaryDataTypeISOGainMap` present (ISO flavor).
    pub iso_aux: bool,
    /// Gain-map plane dimensions as ImageIO reports them.
    pub gain_size: Option<(u32, u32)>,
    /// The plane's `PixelFormat` four-CC, e.g. `L008`.
    pub gain_pixel_format: Option<u32>,
    /// MakerApple tag 33 (`HDRHeadroom`) and tag 48 (`HDRGain`).
    pub tag33: Option<f64>,
    pub tag48: Option<f64>,
    /// Linear headroom implied by the MakerApple tags, via
    /// [`tohdr_core::apple::headroom_from_tags`].
    pub apple_headroom: Option<f32>,
    /// ISO 21496-1 metadata, when a `tmap` payload is present.
    pub iso_meta: Option<GainMapMeta>,
}

impl ReadBack {
    /// Does this file hold the invariant that separates `IMG_4913.HEIC` from the
    /// washed-out exports: declared headroom equal to what the plane encodes?
    ///
    /// `None` when there is no ISO metadata to check.
    pub fn headroom_consistent(&self) -> Option<bool> {
        let m = self.iso_meta.as_ref()?;
        Some((m.max_log2[0] - m.alt_headroom).abs() < 1e-3)
    }
}

/// Ask ImageIO what it sees in a file on disk.
pub fn inspect(path: &Path) -> Result<ReadBack> {
    let _ = path;
    todo!("ImageIO read-back")
}

/// Ask ImageIO what it sees in an in-memory file, so an encode can be verified
/// without touching the filesystem.
pub fn inspect_bytes(bytes: &[u8]) -> Result<ReadBack> {
    let _ = bytes;
    todo!("ImageIO read-back from memory")
}
