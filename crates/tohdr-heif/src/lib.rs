//! ISOBMFF/HEIF reading and gain-map muxing.
//!
//! Engine B owns its container because the structure a gain-map HEIC needs -- a
//! `tmap` derived item, an auxiliary image with Apple's URN, and an `auxl`
//! back-reference over one shared gain-map item -- is exactly what general-purpose
//! HEIF writers do not expose. Reverse-engineered from `IMG_4913.HEIC`; see
//! docs/heic-gainmap-structure.md.
//!
//! Reading covers locating image items, their `hvcC` and coded data, and either
//! gain-map flavour. Not a general decoder: no grid reassembly beyond reporting
//! it, no pixel decoding. Writing covers one base, one gain map, and the boxes
//! tying them together in either or both flavours.

#![forbid(unsafe_code)]

use tohdr_core::{Flavor, GainMapMeta};

mod boxes;
mod engine;
mod read;
mod write;

pub use engine::{chroma_for, coded_image_from_heic, MuxEngine, MuxEngineError, PlaneCodec};

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
    /// Non-image metadata items copied through from a source untouched, each
    /// getting an `infe`, an `idat` extent and a `cdsc` reference to the base and
    /// `tmap` — the same treatment as `Exif` and XMP, because it is the same
    /// relationship: bytes that describe this photograph.
    pub extra_items: Vec<tohdr_core::OpaqueItem>,
    /// `clli` content light level: (max content light level, max frame-average).
    pub clli: Option<(u16, u16)>,
    /// How the stored pixels have to be transformed for display, from the
    /// source's Exif `Orientation`. Applied to *every* image item: the base, the
    /// gain map and the `tmap` are spatially aligned, and a transform on one but
    /// not the others would misregister the map against the image it corrects.
    pub orientation: tohdr_core::HeifTransform,
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
    write::mux(req)
}

/// Apple's auxiliary-image type URN for a gain map, present since 2020.
pub const APPLE_GAINMAP_URN: &str = "urn:com:apple:photo:2020:aux:hdrgainmap";

/// A parsed HEIF file's structure. Borrows the input rather than copying it;
/// item payloads are returned as slices into the original buffer.
#[derive(Debug)]
pub struct HeifFile<'a> {
    pub(crate) bytes: &'a [u8],
    pub(crate) brands: Vec<[u8; 4]>,
    pub(crate) primary_item: Option<ItemId>,
    pub(crate) items: Vec<Item>,
    pub(crate) props: Vec<read::Prop>,
    pub(crate) ipma: Vec<(ItemId, Vec<(u16, bool)>)>,
    pub(crate) iloc: Vec<read::IlocItem>,
    /// Absolute file byte range of `idat`'s body, when present.
    pub(crate) idat: Option<(usize, usize)>,
}

/// One entry from `iinf`, with whatever properties and references we resolved.
#[derive(Clone, Debug)]
pub struct Item {
    pub id: ItemId,
    /// Four-character item type, e.g. `hvc1`, `grid`, `tmap`, `Exif`, `mime`.
    pub item_type: [u8; 4],
    /// `infe`'s `item_name`, empty for most items. Apple's Photographic Styles
    /// plist is named `metadata`.
    pub name: String,
    /// `infe`'s `content_type`, present for `mime` items —
    /// `application/rdf+xml` for an XMP packet.
    pub content_type: Option<String>,
    /// `infe`'s `item_uri_type`, present for `uri ` items and the only thing
    /// that says what such an item holds.
    pub uri_type: Option<String>,
    pub hidden: bool,
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// `auxC` URN, when this item is an auxiliary image.
    pub aux_urn: Option<String>,
    /// Items this one derives from (`dimg`).
    pub derives_from: Vec<ItemId>,
    /// Items this one is auxiliary to (`auxl`).
    pub auxiliary_to: Vec<ItemId>,
    /// Items this one *describes* (`cdsc`). The link that separates metadata
    /// belonging to the photograph from metadata belonging to some auxiliary
    /// image: in `IMG_4913.HEIC` the Exif item and the Photographic Styles plist
    /// both describe the primary, while four XMP items describe mattes.
    pub describes: Vec<ItemId>,
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

// `HeifFile`'s inherent methods (`parse`, `brands`, `primary_item`, `items`,
// `item_data`, `gain_map`, `coded_image`) live in `read.rs`, next to the box
// parsing they depend on.
