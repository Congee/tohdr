//! ISOBMFF/HEIF reading and gain-map muxing.
//!
//! Engine B owns its container rather than delegating to a HEIF library: the
//! structure a gain-map HEIC needs (a `tmap` derived item, an auxiliary image
//! with Apple's URN, and an `auxl` back-reference, all over one shared gain-map
//! image item) is exactly the part general-purpose HEIF writers do not expose.
//! We must own the muxer anyway, so the HEVC encoder stays unmodified and this
//! crate assembles its output.
//!
//! The target structure is not guesswork — it is reverse-engineered byte by byte
//! from `IMG_4913.HEIC`; see `docs/heic-gainmap-structure.md`.
//!
//! # Scope
//!
//! Reading: enough to locate and extract image items, their `hvcC` configuration
//! and coded data, and any gain map (either flavor). Deliberately not a general
//! HEIF decoder — no grid reassembly beyond reporting it, no pixel decoding.
//!
//! Writing: one base image, one gain-map image, and the boxes that tie them
//! together in either or both flavors.

#![forbid(unsafe_code)]

use tohdr_core::{Flavor, GainMapMeta};

/// HEIF item identifier. 16-bit in `infe` version 2, 32-bit in version 3; we
/// model the wider form and narrow on write.
pub type ItemId = u32;

#[derive(Debug)]
pub enum Error {
    /// A box header ran past the end of its parent.
    Truncated { at: usize, need: usize },
    /// A required box was absent.
    MissingBox(&'static str),
    /// Present but unusable, e.g. an `iloc` offset outside the file.
    Malformed(String),
    /// Structurally valid but beyond this crate's scope, e.g. an item stored
    /// with a construction method we do not implement.
    Unsupported(String),
    /// The ISO 21496-1 payload inside a `tmap` failed to parse.
    Metadata(tohdr_core::iso21496::ParseError),
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::Truncated { at, need } => {
                write!(f, "truncated at byte {at}, needed {need} more")
            }
            Error::MissingBox(b) => write!(f, "missing required box: {b}"),
            Error::Malformed(m) => write!(f, "malformed: {m}"),
            Error::Unsupported(m) => write!(f, "unsupported: {m}"),
            Error::Metadata(e) => write!(f, "gain-map metadata: {e}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<tohdr_core::iso21496::ParseError> for Error {
    fn from(e: tohdr_core::iso21496::ParseError) -> Self {
        Error::Metadata(e)
    }
}

pub type Result<T> = core::result::Result<T, Error>;

/// Chroma sampling of a coded image item. A gain map is monochrome; Apple codes
/// it as `L008`, a single 8-bit component.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Chroma {
    Monochrome,
    Yuv420,
    Yuv422,
    Yuv444,
}

/// One coded HEVC image, ready to be placed as a HEIF item.
///
/// `data` is the coded bitstream in HEIF's length-prefixed form (each NAL unit
/// preceded by a 4-byte big-endian length), *not* Annex-B start codes; `hvcc`
/// is the `hvcC` box payload (excluding its own box header) carrying the
/// parameter sets.
#[derive(Clone, Debug)]
pub struct CodedImage {
    pub width: u32,
    pub height: u32,
    pub bit_depth: u8,
    pub chroma: Chroma,
    pub hvcc: Vec<u8>,
    pub data: Vec<u8>,
}

/// Colour information for a `colr` box.
#[derive(Clone, Debug)]
pub enum ColourInfo {
    /// `nclx`: primaries / transfer / matrix indices plus the full-range flag.
    Nclx {
        primaries: u16,
        transfer: u16,
        matrix: u16,
        full_range: bool,
    },
    /// `rICC` or `prof`: an embedded ICC profile.
    Icc(Vec<u8>),
}

/// Everything needed to write a gain-map HEIC.
#[derive(Clone, Debug)]
pub struct MuxRequest {
    /// The SDR base, which becomes the primary item.
    pub base: CodedImage,
    /// The gain map. Typically half the base's resolution and monochrome.
    pub gain: CodedImage,
    pub meta: GainMapMeta,
    pub flavor: Flavor,
    pub base_colour: Option<ColourInfo>,
    /// Colour info for the `tmap` item. `IMG_4913.HEIC` carries a Display-P3
    /// primaries + PQ profile here, describing the *reconstructed* HDR image
    /// rather than the SDR base.
    pub tmap_colour: Option<ColourInfo>,
    /// Exif payload for an `Exif` item, without the leading 4-byte tiff-header
    /// offset (this crate writes it).
    pub exif: Option<Vec<u8>>,
    /// XMP packet for a `mime` item.
    pub xmp: Option<Vec<u8>>,
    /// `clli` content light level: (max content light level, max frame-average).
    pub clli: Option<(u16, u16)>,
}

/// Assemble a gain-map HEIC.
///
/// Emits, per `req.flavor`:
/// - always: `ftyp`, the base image item as `pitm`, the gain-map image item,
///   `ispe`/`pixi`/`hvcC` property associations for both, and `mdat`
/// - [`Flavor::writes_apple`]: an `auxC` on the gain map carrying
///   [`APPLE_GAINMAP_URN`], and an `auxl` `iref` from the gain map to the base
/// - [`Flavor::writes_iso`]: a `tmap` derived item whose `dimg` lists
///   `[base, gain]`, carrying the C.2.2 payload prefixed by its `ToneMapImage`
///   version byte, plus the `tmap` compatible brand in `ftyp`
pub fn mux(req: &MuxRequest) -> Result<Vec<u8>> {
    let _ = req;
    todo!("ISOBMFF gain-map muxer")
}

/// Apple's auxiliary-image type URN for a gain map, present since 2020.
pub const APPLE_GAINMAP_URN: &str = "urn:com:apple:photo:2020:aux:hdrgainmap";

/// A parsed HEIF file's structure. Borrows the input rather than copying it;
/// item payloads are returned as slices into the original buffer.
#[derive(Debug)]
pub struct HeifFile<'a> {
    _bytes: &'a [u8],
}

/// One entry from `iinf`, with whatever properties and references we resolved.
#[derive(Clone, Debug)]
pub struct Item {
    pub id: ItemId,
    /// Four-character item type, e.g. `hvc1`, `grid`, `tmap`, `Exif`, `mime`.
    pub item_type: [u8; 4],
    pub hidden: bool,
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// `auxC` URN, when this item is an auxiliary image.
    pub aux_urn: Option<String>,
    /// Items this one derives from (`dimg`).
    pub derives_from: Vec<ItemId>,
    /// Items this one is auxiliary to (`auxl`).
    pub auxiliary_to: Vec<ItemId>,
}

impl Item {
    pub fn type_str(&self) -> &str {
        core::str::from_utf8(&self.item_type).unwrap_or("????")
    }
}

/// Where a file's gain map lives and how it is signaled. Both flavors can be
/// present at once and, in `IMG_4913.HEIC`, point at the same pixel data.
#[derive(Clone, Debug)]
pub struct GainMapInfo {
    /// The image item holding the gain-map plane.
    pub image_item: ItemId,
    /// Set when an `auxC` with [`APPLE_GAINMAP_URN`] is present.
    pub apple_aux: bool,
    /// The `tmap` derived item, when ISO 21496-1 signaling is present.
    pub tmap_item: Option<ItemId>,
    /// Metadata from the `tmap` payload. `None` for an Apple-only file, whose
    /// parameters live in MakerNote/XMP rather than in the container.
    pub meta: Option<GainMapMeta>,
}

impl<'a> HeifFile<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self> {
        let _ = bytes;
        todo!("ISOBMFF reader")
    }

    /// `ftyp` compatible brands, in file order.
    pub fn brands(&self) -> Vec<[u8; 4]> {
        todo!()
    }

    pub fn primary_item(&self) -> Option<ItemId> {
        todo!()
    }

    pub fn items(&self) -> &[Item] {
        todo!()
    }

    /// An item's payload, with `iloc` extents concatenated. Handles
    /// construction methods 0 (file offset) and 1 (`idat`-relative).
    pub fn item_data(&self, id: ItemId) -> Result<&'a [u8]> {
        let _ = id;
        todo!()
    }

    /// Locate the gain map by either signaling route.
    pub fn gain_map(&self) -> Option<GainMapInfo> {
        todo!()
    }

    /// Pull an item out as a [`CodedImage`], resolving its `hvcC` and `ispe`.
    ///
    /// Fails with [`Error::Unsupported`] for a `grid` item: reassembling tiles
    /// into one coded image is a re-encode, not a remux.
    pub fn coded_image(&self, id: ItemId) -> Result<CodedImage> {
        let _ = id;
        todo!()
    }
}
