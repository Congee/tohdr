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

use tohdr_core::{
    EncodeOptions, GainMapEncoder, GainMapMeta, GainPlane, HdrRgb, Primaries, Rgb,
};

mod read;
mod write;

pub use write::{encode_from_hdr, encode_parts, encode_plane_heic_gray, encode_plane_heic_rgb};

/// An 8-bit `CGImage` in `primaries`, for probes that need a real image to hand a
/// destination. Not part of the encoding surface — `examples/` only.
pub fn cg_image_for_probe(
    rgb: &Rgb,
    primaries: Primaries,
) -> Result<objc2_core_foundation::CFRetained<objc2_core_graphics::CGImage>> {
    write::cg_image_from_sdr(rgb, primaries)
}

pub mod vtenc;
pub use vtenc::CodedPlane;

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

    /// Exif and XMP, but not opaque items.
    ///
    /// Not by writing boxes -- ImageIO owns the container and takes metadata only
    /// as property dictionaries and a `CGImageMetadata`, so what lands in the file
    /// is ImageIO's re-serialisation of the Exif block, not the source's bytes.
    ///
    /// `opaque_items` is `false` because no ImageIO call adds an arbitrary
    /// `infe`/`iloc` item, so Apple's Photographic Styles plist survives only
    /// through our own muxer -- a real difference between the engines, reported
    /// rather than silently dropped.
    ///
    /// `iptc` is `false` on measurement, not on API inspection: ImageIO *reads* the
    /// IIM block back (8 entries, `examples/probe_exif_props.rs`) and its HEIC
    /// writer then emits none. Handing it the dictionary is necessary, not
    /// sufficient.
    fn metadata_support(&self) -> tohdr_core::MetadataSupport {
        tohdr_core::MetadataSupport {
            exif: true,
            xmp: true,
            iptc: false,
            opaque_items: false,
            // ImageIO's property model has `kCGImagePropertyMakerAppleDictionary`
            // and no key for a raw vendor blob, so an iPhone's `MakerNote`
            // round-trips and a Sony one does not survive being turned into
            // properties and back. Measured: a block carrying the byte-identical
            // 38,332-byte Sony blob out of a 60 MP Sony raw yields 0 `MakerNote`
            // tags here and 124 through Engine B.
            maker_note: false,
            // The block reaches ImageIO inside a JPEG carrier, whose `APP1`
            // length is 16 bits — see `write::exif_property_pairs`, which gets
            // no properties back at all from a block that will not fit one.
            max_exif_block: Some(tohdr_core::exif::MAX_BLOCK),
        }
    }

    fn encode(
        &self,
        base: &Rgb,
        gain: &GainPlane,
        meta: &GainMapMeta,
        opts: &EncodeOptions,
    ) -> Result<Vec<u8>> {
        write::encode_parts(base, gain, meta, opts)
    }
}

/// Decode any ImageIO-supported file into linear extended-range HDR.
///
/// Handles the two input shapes that matter: a plain HDR file (16-bit TIFF, EXR,
/// HDR-tagged PNG/JPEG) and an existing gain-map HEIC, where the HDR image is
/// the *reconstruction* of base plus gain map rather than anything stored
/// directly. `1.0` in the result is SDR diffuse white.
pub fn load_hdr(path: &Path) -> Result<HdrRgb> {
    load_hdr_in(path, Primaries::Bt709)
}

/// [`load_hdr`] into a chosen set of primaries.
///
/// The choice is lossy and it is made here, not later: ImageIO renders into the
/// space asked for and carries anything outside its primaries as negative
/// components, which this loader clamps away. So the *narrow* request is the
/// destructive one, and there is no recovering from it downstream — see
/// [`tohdr_core::colour`] for what it costs on real files.
pub fn load_hdr_in(path: &Path, primaries: Primaries) -> Result<HdrRgb> {
    read::load_hdr(path, primaries)
}

/// Decode the SDR base of a file, without applying any gain map.
pub fn load_sdr(path: &Path) -> Result<Rgb> {
    load_sdr_in(path, Primaries::Bt709)
}

/// [`load_sdr`] into a chosen set of primaries.
pub fn load_sdr_in(path: &Path, primaries: Primaries) -> Result<Rgb> {
    read::load_sdr(path, primaries)
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
    /// The display transform ImageIO resolved from the container's `irot`/`imir`,
    /// expressed as an Exif `Orientation` (`1..=8`). `None` when the file states
    /// none.
    ///
    /// The oracle for orientation passthrough: a source whose Exif said `6` must
    /// read back as `6` here, or a rotated photo comes out sideways in every
    /// Apple app.
    pub orientation: Option<u32>,
}

impl ReadBack {
    /// Does this file hold the invariant that separates the reference capture from the
    /// washed-out exports: declared headroom equal to what the plane encodes?
    ///
    /// `None` when there is no ISO metadata to check.
    ///
    /// `max_log2` is floored at zero before the comparison because the ISO
    /// headroom fields are *unsigned* (libavif `include/avif/avif.h:692-693`),
    /// so a plane that only darkens — `max_log2 < 0` — can only ever declare
    /// `alt_headroom = 0`. Comparing the raw values would fail such a file for
    /// being encoded the only way it can be. See
    /// `tohdr_core::hdr::derive_consistent`, which produces the floored pair.
    pub fn headroom_consistent(&self) -> Option<bool> {
        let m = self.iso_meta.as_ref()?;
        Some((m.max_log2[0].max(0.0) - m.alt_headroom).abs() < 1e-3)
    }
}

/// Ask ImageIO what it sees in a file on disk.
pub fn inspect(path: &Path) -> Result<ReadBack> {
    read::inspect_path(path)
}

/// Ask ImageIO what it sees in an in-memory file, so an encode can be verified
/// without touching the filesystem.
pub fn inspect_bytes(bytes: &[u8]) -> Result<ReadBack> {
    read::inspect_bytes(bytes)
}
