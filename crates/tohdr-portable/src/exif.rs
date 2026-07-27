//! Lifting a source file's Exif block out, so a conversion keeps the camera,
//! lens, exposure and date.
//!
//! Every supported source carries Exif as one contiguous TIFF structure (a HEIF
//! `Exif` item, a JPEG `APP1` payload, or a Lightroom TIFF's own `IFD0`), so this
//! finds that structure and re-emits it standalone for
//! [`tohdr_heif::MuxRequest::exif`]. Selection is a *denylist*, so an unknown tag
//! is carried rather than lost.
//!
//! It is rebuilt rather than copied for two reasons:
//!
//! - A TIFF `IFD0` describes pixels too. Its `StripOffsets` and `SubIFDs` point
//!   into a file the output does not contain, so copying them yields live
//!   pointers into nothing. That is what [`IFD0_DROP`] removes, and why the list
//!   is by tag number: only we know those offsets no longer lead anywhere.
//! - A value addressed relative to the TIFF header cannot move. `MakerNote`
//!   (`0x927C`) is the one that matters -- Apple's, Canon's and Sony's all use
//!   offsets into the enclosing TIFF -- so [`serialize`] pads it back to the exact
//!   block-relative offset the source had, and vendor detection is never needed.
//!
//! ICC (`0x8773`) and an `IFD0` XMP packet (`0x02BC`) are left out for a different
//! reason: the `colr` box and the XMP item carry them better.
//!
//! A rendered intermediate has no `MakerNote` at all, so [`read_maker_note`]
//! reads the original camera file a second time for that one tag and
//! [`read_with_maker_note`] grafts it in with the same pinning. See
//! lightroom/README.md for what a render does and does not forward.

use std::path::Path;

use crate::gainmap_tiff::{type_size, Ifd, Tiff};
use crate::Result;

/// Which container the block came from. Reported by the CLI so "the source had
/// no Exif" is distinguishable from "we dropped it".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    /// An `Exif` item in a HEIF/HEIC source.
    Heif,
    /// A JPEG `APP1` segment.
    Jpeg,
    /// A TIFF's own `IFD0` — the Lightroom Classic path.
    Tiff,
}

impl core::fmt::Display for Origin {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Origin::Heif => "heif-exif-item",
            Origin::Jpeg => "jpeg-app1",
            Origin::Tiff => "tiff-ifd0",
        })
    }
}

/// A rebuilt Exif block, ready to hand to a muxer.
#[derive(Clone, Debug)]
pub struct SourceExif {
    /// A complete TIFF structure: 8-byte header, `IFD0`, the sub-IFDs it points
    /// at, `IFD1` when there is a thumbnail, then their values. In the
    /// *source's* byte order, which is what lets every value be copied verbatim.
    pub tiff: Vec<u8>,
    pub origin: Origin,
    /// How many entries survived, excluding the sub-IFD pointers. A `MakerNote`
    /// counts once here however many vendor tags a reader finds inside it, so
    /// this sits below `exiftool`'s total by exactly that much.
    pub tag_count: usize,
    /// `Orientation` (`0x0112`), `1` when the source states none.
    ///
    /// Reported separately as well as carried in the block because the container
    /// has to declare the same transform: an Exif reader and a HEIF reader
    /// consult different fields, and a file where those disagree is a file where
    /// two conformant viewers show the photo different ways up.
    pub orientation: u8,
    /// The IPTC-IIM block (`IFD0` tag `33723`), when the source has one.
    ///
    /// Also carried inside [`SourceExif::tiff`] as that tag; handed out
    /// separately because a backend writing through a JPEG carrier has to put it
    /// in an `APP13` segment instead — see
    /// [`tohdr_core::exif::wrap_in_jpeg_with_iptc`].
    pub iptc: Option<Vec<u8>>,
    /// Set when the source had a `MakerNote` that could not be pinned to its
    /// original offset, so it was left out. False for every real capture
    /// measured here; reported rather than hidden because the alternative to
    /// dropping it is emitting bytes that no longer parse.
    pub dropped_maker_note: bool,
    /// What became of a companion file's `MakerNote`, when one was offered.
    /// [`MakerNoteGraft::NotOffered`] from [`read`], which offers none.
    pub maker_note_graft: MakerNoteGraft,
}

/// A `MakerNote` lifted out of one file so it can be grafted into another's Exif
/// block. See the module docs for why a rendered intermediate needs this.
#[derive(Clone, Debug)]
pub struct ForeignMakerNote {
    bytes: Vec<u8>,
    /// The entry's declared type and count, carried rather than assumed: a
    /// vendor that writes something other than `UNDEFINED` keeps its own claim.
    typ: u16,
    count: u32,
    /// Where it sat, measured from the TIFF header of the block it came out of —
    /// the file's first byte, for a raw. Kept because the blob's own contents
    /// are addressed against it, and pinning is what makes them true again.
    offset: usize,
    /// Byte order of the block it came out of.
    ///
    /// Most vendor `MakerNote`s carry no byte-order mark of their own — Sony's
    /// begins with its entry count and nothing else — so a reader takes the
    /// order from the enclosing block. Grafting one into a block of the opposite
    /// order would transpose every value it holds while parsing cleanly, which
    /// is why [`MakerNoteGraft::ByteOrderDiffers`] refuses instead.
    little_endian: bool,
}

impl ForeignMakerNote {
    /// Size of the blob.
    ///
    /// The only honest measure of it here: how many tags a reader finds inside
    /// depends on vendor layout this module deliberately does not interpret.
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// The offset the blob's own pointers are written against.
    pub fn offset(&self) -> usize {
        self.offset
    }

    /// Whether this is Apple's `MakerNote`.
    ///
    /// The one vendor's block a backend writing through macOS ImageIO can still
    /// carry, since ImageIO has a property key for it and none for anyone
    /// else's — so a caller checking
    /// [`tohdr_core::MetadataSupport::maker_note`] needs this to tell "the
    /// engine will drop this" from "the engine will drop everything but this".
    pub fn is_apple(&self) -> bool {
        self.bytes.starts_with(APPLE_MAKER_SIG)
    }
}

/// What [`read_with_maker_note`] did with the companion `MakerNote` it was given.
///
/// Every refusal is a distinct reason, and each is reported rather than folded
/// into a single "no": grafting the wrong bytes produces a file that parses and
/// lies, so which check stopped it is the useful half of the answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MakerNoteGraft {
    /// No companion `MakerNote` was supplied.
    NotOffered,
    /// Grafted, pinned at the offset its contents are addressed against.
    Carried { bytes: usize },
    /// The source block has a `MakerNote` of its own, which wins — it is the one
    /// the rest of that block was written alongside.
    HostHasOwn,
    /// The two blocks disagree about byte order. See
    /// [`ForeignMakerNote::little_endian`].
    ByteOrderDiffers,
    /// Its offset falls inside the rebuilt block's IFD region, or so far past it
    /// that [`MAX_PIN_PADDING`] of padding would not reach.
    Unreachable,
    /// The source block has no Exif IFD, which is the only IFD a `MakerNote`
    /// belongs in. Inventing one to hold a foreign tag would put a sub-IFD in a
    /// file whose source never had one.
    NoExifIfd,
}

impl core::fmt::Display for MakerNoteGraft {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            MakerNoteGraft::NotOffered => "not-offered",
            MakerNoteGraft::Carried { .. } => "carried",
            MakerNoteGraft::HostHasOwn => "source-has-own",
            MakerNoteGraft::ByteOrderDiffers => "byte-order-differs",
            MakerNoteGraft::Unreachable => "unreachable-offset",
            MakerNoteGraft::NoExifIfd => "no-exif-ifd",
        })
    }
}

/// `0x0112`.
const TAG_ORIENTATION: u16 = 274;
/// `0x0201`/`0x0202`, the `IFD1` thumbnail's offset and length.
const TAG_THUMBNAIL_OFFSET: u16 = 513;
const TAG_THUMBNAIL_LENGTH: u16 = 514;
/// `0x8769`, the pointer to Exif's own IFD.
const TAG_EXIF_IFD: u16 = 34665;
/// `0x8825`, the pointer to the GPS IFD.
const TAG_GPS_IFD: u16 = 34853;
/// `0x927C`, whose contents address themselves — see the module docs.
const TAG_MAKER_NOTE: u16 = 37500;
/// `0xA005`, the pointer to the Interoperability IFD.
const TAG_INTEROP_IFD: u16 = 40965;

/// Tags that must not be copied into the output's `IFD0`.
///
/// A denylist, where the sub-IFDs keep everything: `IFD0` is the one IFD a TIFF
/// source uses to describe its own pixels, so it is the only one where a
/// verbatim copy can produce a dangling pointer or a false claim. Three classes,
/// and the reason differs for each.
const IFD0_DROP: &[u16] = &[
    // 1. Offsets and lengths that address bytes the output does not contain.
    //    A reader that knows these tags *will* follow them.
    273,   // StripOffsets
    279,   // StripByteCounts
    288,   // FreeOffsets
    289,   // FreeByteCounts
    324,   // TileOffsets
    325,   // TileByteCounts
    330,   // SubIFDs — reaches Lightroom's gain map
    513,   // JPEGInterchangeFormat: `IFD1`'s copy is relocated by `plan_thumbnail`
    514,   // JPEGInterchangeFormatLength
    519,   // JPEGQTables
    520,   // JPEGDCTables
    521,   // JPEGACTables
    559,   // StripRowCounts
    50740, // DNGPrivateData — an opaque blob with vendor offsets inside it
    // 2. How the *source's* pixels were laid out. The output's are 8-bit YCbCr
    //    in HEVC whatever arrived, so copying these would state something false
    //    rather than lose something true.
    254, 255, // NewSubfileType, SubfileType
    258, // BitsPerSample
    259, // Compression
    262, // PhotometricInterpretation
    263, 264, 265, 266, // Threshholding, CellWidth, CellLength, FillOrder
    277, // SamplesPerPixel
    278, // RowsPerStrip
    284, // PlanarConfiguration
    290, 291, // GrayResponseUnit, GrayResponseCurve
    292, 293, // T4Options, T6Options
    301, // TransferFunction
    317, // Predictor
    320, // ColorMap
    322, 323, // TileWidth, TileLength
    332, 333, 334, 336, 337, // Ink*, DotRange, TargetPrinter
    338, 339, 340, 341, 342, // ExtraSamples, SampleFormat, S{Min,Max}Value, TransferRange
    512, 515, 517, 518, // JPEGProc, RestartInterval, LosslessPredictors, PointTransforms
    532,   // ReferenceBlackWhite
    50706, // DNGVersion — the output is not a DNG
    50707, // DNGBackwardVersion
    // 3. Carried by a part of the output better suited to it, so keeping the
    //    `IFD0` copy would put two statements of one fact in one file.
    700,   // XMP — gets its own item
    34675, // InterColorProfile — the `colr` box's job
];

/// Beyond this much padding, pinning a `MakerNote` costs more than the tag is
/// worth. Reached only by a source whose Exif block is mostly values we drop;
/// the reference capture needs 12 bytes.
const MAX_PIN_PADDING: usize = 1 << 20;

/// Apple's `MakerNote` signature. Two bytes after it comes its own byte-order
/// mark, and every offset inside it is relative to the `A` of `Apple` — which is
/// exactly what [`serialize`]'s pinning preserves.
const APPLE_MAKER_SIG: &[u8] = b"Apple iOS\0\0";
/// Distance from an Apple `MakerNote`'s first byte to its IFD: the signature, a
/// version byte, and the byte-order mark. Measured on the reference capture, where the
/// 59-entry IFD at `+14` ends exactly where its first value begins.
const APPLE_MAKER_IFD_AT: usize = 14;
/// `HDRHeadroom` and `HDRGain` inside Apple's `MakerNote`.
const APPLE_TAG_HEADROOM: u16 = 33;
const APPLE_TAG_GAIN: u16 = 48;
/// The tolerance `docs/acceptance-criteria.md` §9 sets for two copies of the
/// headroom in one file.
const HEADROOM_COPIES_TOLERANCE: f32 = 1e-3;

/// What [`align_apple_headroom`] did to the carried `MakerNote`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppleHeadroom {
    /// No Apple `MakerNote`, or one without the headroom pair. Nothing to do.
    Absent,
    /// Tag 48 now encodes *this* output's headroom rather than the source's.
    Rewritten,
    /// Both tags removed, because no tag 48 can express a headroom this large
    /// without understating it. `docs/acceptance-criteria.md` §8 spells out why
    /// silence is the only option that never lies.
    Removed,
}

/// Make the carried `MakerNote`'s headroom agree with the output's.
///
/// Headroom is stated three times -- ISO payload, XMP, MakerApple 33/48 -- and all
/// copies in one file must agree within 1e-3 (docs/acceptance-criteria.md 9), or a
/// consumer reads the wrong number. The source's tag 48 describes the *source's*
/// headroom, 0.019 stops off ours on the reference capture, so carrying it verbatim
/// imports a 1.3% over-declaration.
///
/// Only tag 48 is rewritten: `headroom_from_tags` uses tag 33 solely to pick a
/// branch at the 1.0 threshold, so a value already above 1.0 is left as the camera
/// wrote it. Both keep their denominators and offsets, so the note's length never
/// changes and the pin stays valid.
pub fn align_apple_headroom(block: &mut [u8], headroom_linear: f32) -> AppleHeadroom {
    let Some((start, len)) = locate_apple_maker_note(block) else {
        return AppleHeadroom::Absent;
    };
    let note = &block[start..start + len];
    let le = note.get(12..14) == Some(b"II");
    let Some(entries) = apple_maker_entries(note, le) else {
        return AppleHeadroom::Absent;
    };
    let find = |tag: u16| {
        entries
            .iter()
            .copied()
            .find(|(t, ..)| *t == tag)
            // A RATIONAL is the only shape either tag is written in, and at 8
            // bytes it is always out of line, so a value offset is a value
            // offset and not four inline bytes misread as one.
            .filter(|(_, typ, count, _, _)| matches!(typ, 5 | 10) && *count == 1)
    };
    let (Some(t33), Some(t48)) = (find(APPLE_TAG_HEADROOM), find(APPLE_TAG_GAIN)) else {
        return AppleHeadroom::Absent;
    };

    let rational = |rel: usize| -> Option<(i32, i32)> {
        let v = note.get(rel..rel + 8)?;
        let rd = |b: &[u8]| {
            let a = [b[0], b[1], b[2], b[3]];
            if le {
                i32::from_le_bytes(a)
            } else {
                i32::from_be_bytes(a)
            }
        };
        Some((rd(&v[0..4]), rd(&v[4..8])))
    };
    let (Some((n33, d33)), Some((_, d48))) = (rational(t33.3), rational(t48.3)) else {
        return AppleHeadroom::Absent;
    };

    let (want33, want48) = tohdr_core::apple::tags_from_headroom(headroom_linear);
    // Quantizing to a rational is lossy, so check the *written* number rather
    // than the ideal one: this is criterion 9's own test, run before the bytes
    // go in rather than after.
    let d48 = if d48 <= 0 || want48 * d48 as f64 > i32::MAX as f64 {
        1_000_000
    } else {
        d48
    };
    let num48 = (want48 * d48 as f64).round() as i32;
    let source_tag33 = if d33 == 0 { want33 } else { n33 as f64 / d33 as f64 };
    let raise_tag33 = source_tag33 < 1.0;
    let effective33 = if raise_tag33 { want33 } else { source_tag33 };
    let decoded = tohdr_core::apple::headroom_from_tags(effective33, num48 as f64 / d48 as f64);

    if (decoded - headroom_linear).abs() >= HEADROOM_COPIES_TOLERANCE {
        return if remove_apple_entries(block, start, le, &[APPLE_TAG_HEADROOM, APPLE_TAG_GAIN]) {
            AppleHeadroom::Removed
        } else {
            AppleHeadroom::Absent
        };
    }

    let put = |block: &mut [u8], rel: usize, num: i32, den: i32| {
        let at = start + rel;
        let (n, d) = if le {
            (num.to_le_bytes(), den.to_le_bytes())
        } else {
            (num.to_be_bytes(), den.to_be_bytes())
        };
        block[at..at + 4].copy_from_slice(&n);
        block[at + 4..at + 8].copy_from_slice(&d);
    };
    put(block, t48.3, num48, d48);
    if raise_tag33 {
        let d = if d33 <= 0 { 1_000_000 } else { d33 };
        put(block, t33.3, d, d);
    }
    AppleHeadroom::Rewritten
}

/// Byte range of an Apple `MakerNote` inside an Exif block.
fn locate_apple_maker_note(block: &[u8]) -> Option<(usize, usize)> {
    let tiff = Tiff::open(block).ok()??;
    let ifd0 = tiff.read_ifd(tiff.first_ifd).ok()?;
    let exif_ifd = tiff.read_ifd(sub_ifd_offset(&tiff, &ifd0, TAG_EXIF_IFD)?).ok()?;
    let e = exif_ifd.get(TAG_MAKER_NOTE)?;
    let bytes = tiff.bytes_of(&e).ok()?;
    if !bytes.starts_with(APPLE_MAKER_SIG) {
        return None;
    }
    Some((e.value_off, bytes.len()))
}

/// `(tag, type, count, value offset relative to the note's start, entry offset)`
/// for every entry of an Apple `MakerNote`'s IFD.
///
/// Its own reader rather than [`Tiff`]'s because the offsets inside it are
/// relative to the `MakerNote`, not to the enclosing TIFF header — the very
/// property that makes the note unrelocatable.
#[allow(clippy::type_complexity)]
fn apple_maker_entries(note: &[u8], le: bool) -> Option<Vec<(u16, u16, u32, usize, usize)>> {
    let u16at = |at: usize| -> Option<u16> {
        let b = note.get(at..at + 2)?;
        let a = [b[0], b[1]];
        Some(if le {
            u16::from_le_bytes(a)
        } else {
            u16::from_be_bytes(a)
        })
    };
    let u32at = |at: usize| -> Option<u32> {
        let b = note.get(at..at + 4)?;
        let a = [b[0], b[1], b[2], b[3]];
        Some(if le {
            u32::from_le_bytes(a)
        } else {
            u32::from_be_bytes(a)
        })
    };
    let n = u16at(APPLE_MAKER_IFD_AT)? as usize;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let e = APPLE_MAKER_IFD_AT + 2 + i * 12;
        let (tag, typ, count) = (u16at(e)?, u16at(e + 2)?, u32at(e + 4)?);
        let size = type_size(typ) * count as usize;
        let rel = if size <= 4 { e + 8 } else { u32at(e + 8)? as usize };
        out.push((tag, typ, count, rel, e));
    }
    Some(out)
}

/// Drop entries from an Apple `MakerNote`'s IFD without moving anything else.
///
/// Compacting the entry array and decrementing the count is enough: TIFF values
/// are reached by explicit offset, so the survivors' values stay exactly where
/// they were, and the bytes freed at the end of the array become slack that
/// nothing points into. The note's length is unchanged, which is what keeps the
/// outer entry's byte count and the pinned offset true.
fn remove_apple_entries(block: &mut [u8], start: usize, le: bool, tags: &[u16]) -> bool {
    let Some(entries) = apple_maker_entries(&block[start..], le) else {
        return false;
    };
    let kept: Vec<usize> = entries
        .iter()
        .filter(|(t, ..)| !tags.contains(t))
        .map(|(.., e)| *e)
        .collect();
    if kept.len() == entries.len() {
        return false;
    }
    let ifd = start + APPLE_MAKER_IFD_AT;
    let old_next = ifd + 2 + entries.len() * 12;
    let Some(next_field) = block.get(old_next..old_next + 4).map(<[u8]>::to_vec) else {
        return false;
    };
    let packed: Vec<u8> = kept
        .iter()
        .flat_map(|e| block[start + e..start + e + 12].to_vec())
        .collect();
    let n = kept.len() as u16;
    block[ifd..ifd + 2].copy_from_slice(&if le {
        n.to_le_bytes()
    } else {
        n.to_be_bytes()
    });
    block[ifd + 2..ifd + 2 + packed.len()].copy_from_slice(&packed);
    let new_next = ifd + 2 + packed.len();
    block[new_next..new_next + 4].copy_from_slice(&next_field);
    true
}

/// Reads `path`'s Exif block, or `Ok(None)` if it has none.
///
/// `Ok(None)` covers both "this format carries no Exif" and "this file's Exif
/// is empty", neither of which is a reason to fail a conversion.
pub fn read(path: &Path) -> Result<Option<SourceExif>> {
    read_with_maker_note(path, None)
}

/// [`read`], with `companion`'s `MakerNote` grafted into the block.
///
/// `companion` comes from [`read_maker_note`] on the original camera file. The
/// graft is attempted, not assumed: [`SourceExif::maker_note_graft`] says what
/// happened, and every refusal leaves the rest of the block exactly as [`read`]
/// would have produced it.
pub fn read_with_maker_note(
    path: &Path,
    companion: Option<&ForeignMakerNote>,
) -> Result<Option<SourceExif>> {
    let bytes = std::fs::read(path)?;
    read_bytes_with_maker_note(&bytes, companion)
}

/// [`read`] on an in-memory file, so tests need no filesystem.
pub fn read_bytes(bytes: &[u8]) -> Result<Option<SourceExif>> {
    read_bytes_with_maker_note(bytes, None)
}

/// [`read_with_maker_note`] on an in-memory file.
pub fn read_bytes_with_maker_note(
    bytes: &[u8],
    companion: Option<&ForeignMakerNote>,
) -> Result<Option<SourceExif>> {
    let Some((block, origin)) = locate(bytes)? else {
        return Ok(None);
    };
    // A JPEG keeps IPTC in an `APP13` segment, outside the Exif block entirely,
    // so a reader that only walks the block loses the photographer's creator,
    // rights and keywords. Lifted here and folded into `IFD0` below, which is
    // where a HEIF reader looks for it.
    let external_iptc = tohdr_core::exif::app13_iptc_payload(bytes);
    // A source can carry an Exif item that holds nothing we keep, which is not
    // an error — it is the same outcome as carrying none.
    match rebuild(block, external_iptc, companion)? {
        Some(r) => Ok(Some(SourceExif {
            tiff: r.tiff,
            origin,
            tag_count: r.tag_count,
            iptc: r.iptc,
            orientation: r.orientation,
            dropped_maker_note: r.dropped_maker_note,
            maker_note_graft: r.maker_note_graft,
        })),
        None => Ok(None),
    }
}

/// How much of a companion file's head is read looking for its `MakerNote`.
///
/// The point of reading the original at all is to *not* read the whole thing:
/// a 60 MP Sony raw is 71,708,672 bytes and holds `IFD0` at 8, its Exif IFD at
/// 4,544 and the `MakerNote` at 5,222 — a targeted read of that range measures
/// 0.00 ms. A camera writes its IFDs ahead of its image data because the image
/// data is the part that is big, so this ceiling clears the real thing by three
/// orders of magnitude and still bounds the read.
const COMPANION_HEAD: u64 = 1 << 20;

/// The `MakerNote` of `path`, for grafting into another file's Exif block.
///
/// `Ok(None)` when the file has none, which is not an error: a JPEG out of
/// Lightroom, a scan, a synthetic image all legitimately have none.
///
/// Reads [`COMPANION_HEAD`] bytes and falls back to the whole file only when
/// that head says a `MakerNote` exists somewhere it cannot reach — so the fast
/// path is bounded and the unusual one is still right, rather than one of the two
/// at the other's expense.
pub fn read_maker_note(path: &Path) -> Result<Option<ForeignMakerNote>> {
    use std::io::Read;

    let mut head = Vec::new();
    std::fs::File::open(path)?
        .take(COMPANION_HEAD)
        .read_to_end(&mut head)?;
    let whole_file = (head.len() as u64) < COMPANION_HEAD;

    match maker_note_in(&head) {
        Located::Found(note) => Ok(Some(note)),
        Located::Absent => Ok(None),
        // Already had every byte there is, so re-reading would ask the same
        // question of the same bytes.
        Located::Beyond if whole_file => Ok(None),
        Located::Beyond => Ok(match maker_note_in(&std::fs::read(path)?) {
            Located::Found(note) => Some(note),
            Located::Absent | Located::Beyond => None,
        }),
    }
}

/// [`read_maker_note`] on an in-memory file, so a caller that already has the
/// bytes — or a test — needs no filesystem.
///
/// `None` covers both "no `MakerNote`" and "these bytes do not reach it"; use
/// [`read_maker_note`] to have the second one resolved by reading more.
pub fn maker_note_from_bytes(bytes: &[u8]) -> Option<ForeignMakerNote> {
    match maker_note_in(bytes) {
        Located::Found(note) => Some(note),
        Located::Absent | Located::Beyond => None,
    }
}

/// Whether a slice holds a `MakerNote`, or why the answer is not in this slice.
enum Located {
    Found(ForeignMakerNote),
    /// The block was read and states no `MakerNote`. A real answer.
    Absent,
    /// Something ran past the end of this slice. Undecided, not negative — which
    /// is the distinction that keeps [`read_maker_note`] from paying for a
    /// whole-file read on every source that simply has no `MakerNote`.
    Beyond,
}

fn maker_note_in(bytes: &[u8]) -> Located {
    let Ok(Some((block, _))) = locate(bytes) else {
        return Located::Beyond;
    };
    let Ok(Some(tiff)) = Tiff::open(block) else {
        return Located::Beyond;
    };
    let Ok(ifd0) = tiff.read_ifd(tiff.first_ifd) else {
        return Located::Beyond;
    };
    let Some(off) = sub_ifd_offset(&tiff, &ifd0, TAG_EXIF_IFD) else {
        return Located::Absent;
    };
    let Ok(exif_ifd) = tiff.read_ifd(off) else {
        return Located::Beyond;
    };
    let Some(e) = exif_ifd.get(TAG_MAKER_NOTE) else {
        return Located::Absent;
    };
    // An unrecognized type has no element size, so the value's length is
    // unknowable — the same reason `plan` drops one rather than guessing.
    if type_size(e.typ) == 0 {
        return Located::Absent;
    }
    let Ok(blob) = tiff.bytes_of(&e) else {
        return Located::Beyond;
    };
    if blob.is_empty() {
        return Located::Absent;
    }
    Located::Found(ForeignMakerNote {
        bytes: blob.to_vec(),
        typ: e.typ,
        count: e.count,
        offset: e.value_off,
        little_endian: tiff.little_endian,
    })
}

/// Find the source's Exif TIFF structure without interpreting it.
fn locate(bytes: &[u8]) -> Result<Option<(&[u8], Origin)>> {
    if bytes.len() < 12 {
        return Ok(None);
    }
    // TIFF's own magic is the block's magic: a TIFF source *is* the block.
    if matches!(&bytes[0..2], b"II" | b"MM") {
        return Ok(Some((bytes, Origin::Tiff)));
    }
    if bytes[0..2] == [0xff, 0xd8] {
        return Ok(tohdr_core::exif::app1_payload(bytes).map(|b| (b, Origin::Jpeg)));
    }
    if &bytes[4..8] == b"ftyp" {
        return Ok(heif_exif_item(bytes)?.map(|b| (b, Origin::Heif)));
    }
    Ok(None)
}

/// The payload of a HEIF `Exif` item, past the `exif_tiff_header_offset`.
fn heif_exif_item(bytes: &[u8]) -> Result<Option<&[u8]>> {
    // A source that is not parseable as HEIF is not an Exif failure: the caller
    // is about to decode it and will report the real problem. Losing metadata is
    // the lesser outcome than failing the conversion here.
    let Ok(file) = tohdr_heif::HeifFile::parse(bytes) else {
        return Ok(None);
    };
    let Some(item) = file.items().iter().find(|i| i.item_type == *b"Exif") else {
        return Ok(None);
    };
    let Ok(data) = file.item_data(item.id) else {
        return Ok(None);
    };
    // ISO/IEC 23008-12 A.2.1: a big-endian u32 count of bytes to skip between
    // the field and the TIFF header. Apple writes 0.
    if data.len() < 4 {
        return Ok(None);
    }
    let skip = u32::from_be_bytes([data[0], data[1], data[2], data[3]]) as usize;
    Ok(data.get(4 + skip..).filter(|b| b.len() >= 8))
}

/// One entry's value.
enum Value<'a> {
    /// Copied from the source; stored in the entry itself when it fits in four
    /// bytes, in the value area otherwise.
    Bytes(&'a [u8]),
    /// Four bytes computed here rather than copied, always inside the entry.
    Immediate([u8; 4]),
    /// Copied from the source and placed at exactly `offset`, because the
    /// value's own contents are addressed relative to the TIFF header.
    Pinned { bytes: &'a [u8], offset: usize },
    /// Placed wherever there is room, with the entry holding a LONG *pointing*
    /// at it: the `IFD1` thumbnail, which is the one value TIFF addresses this
    /// way rather than storing inline.
    DataOffset(&'a [u8]),
    /// Index into the serialized IFD list, in which 0 is always `IFD0`.
    IfdPointer(usize),
}

struct PlanEntry<'a> {
    tag: u16,
    typ: u16,
    count: u32,
    value: Value<'a>,
}

struct PlannedIfd<'a> {
    entries: Vec<PlanEntry<'a>>,
    /// Index of the IFD this one chains to via its trailing `next_ifd` field.
    /// Only ever `IFD0` -> `IFD1`.
    next: Option<usize>,
}

/// `0x83BB`, the IPTC-IIM block.
const TAG_IPTC: u16 = 33723;

struct Rebuilt {
    tiff: Vec<u8>,
    tag_count: usize,
    iptc: Option<Vec<u8>>,
    orientation: u8,
    dropped_maker_note: bool,
    maker_note_graft: MakerNoteGraft,
}

/// A LONG-typed pointer entry, which fits in the entry itself and so needs no
/// space in the value area.
fn pointer(tag: u16, target: usize) -> PlanEntry<'static> {
    PlanEntry {
        tag,
        typ: 4,
        count: 1,
        value: Value::IfdPointer(target),
    }
}

/// The offset a sub-IFD pointer names, or `None` if the tag is absent or does
/// not hold a usable one.
fn sub_ifd_offset(tiff: &Tiff, ifd: &Ifd, tag: u16) -> Option<usize> {
    let entry = ifd.get(tag)?;
    let offsets = tiff.integers(&entry).ok()?;
    offsets.first().map(|&o| o as usize)
}

/// Re-emit `block` as a standalone Exif TIFF.
///
/// `Ok(None)` when nothing survived, which is the same outcome for the caller as
/// a source with no Exif at all.
fn rebuild<'a>(
    block: &'a [u8],
    external_iptc: Option<&'a [u8]>,
    companion: Option<&'a ForeignMakerNote>,
) -> Result<Option<Rebuilt>> {
    let Some(tiff) = Tiff::open(block)? else {
        return Ok(None);
    };
    let ifd0 = tiff.read_ifd(tiff.first_ifd)?;

    let orientation = ifd0
        .get(TAG_ORIENTATION)
        .and_then(|e| tiff.integers(&e).ok())
        .and_then(|v| v.first().copied())
        .filter(|v| (1..=8).contains(v))
        .unwrap_or(1) as u8;

    // Slot 0 is reserved for `IFD0`, so its pointer entries can name the
    // sub-IFDs by index as those are pushed.
    let mut ifds: Vec<PlannedIfd> = vec![PlannedIfd {
        entries: Vec::new(),
        next: None,
    }];
    let mut root = plan(&tiff, &ifd0, |t| {
        !IFD0_DROP.contains(&t) && !matches!(t, TAG_EXIF_IFD | TAG_GPS_IFD)
    })?;
    // The source's own tag wins: it is the one the rest of its Exif was written
    // against, and a JPEG that has both is stating the same thing twice.
    if ifd0.get(TAG_IPTC).is_none() {
        if let Some(iim) = external_iptc.filter(|b| !b.is_empty()) {
            root.push(PlanEntry {
                tag: TAG_IPTC,
                typ: 7,
                count: iim.len() as u32,
                value: Value::Bytes(iim),
            });
        }
    }
    // Where the tentative `MakerNote` entry ended up, so it can be withdrawn if
    // its offset turns out to be unreachable, and whether that entry is the
    // source's own or a graft — withdrawing them means reporting two different
    // things.
    let mut maker_at: Option<(usize, usize)> = None;
    let mut maker_is_graft = false;
    let mut graft = MakerNoteGraft::NotOffered;

    if let Some(off) = sub_ifd_offset(&tiff, &ifd0, TAG_EXIF_IFD) {
        if let Ok(exif_ifd) = tiff.read_ifd(off) {
            // The Exif IFD holds two things that are not plain values: the
            // Interoperability pointer and the MakerNote.
            let mut entries = plan(&tiff, &exif_ifd, |t| {
                !matches!(t, TAG_INTEROP_IFD | TAG_MAKER_NOTE)
            })?;
            if let Some(ioff) = sub_ifd_offset(&tiff, &exif_ifd, TAG_INTEROP_IFD) {
                if let Ok(interop) = tiff.read_ifd(ioff) {
                    let kept = plan(&tiff, &interop, |_| true)?;
                    if !kept.is_empty() {
                        ifds.push(PlannedIfd {
                            entries: kept,
                            next: None,
                        });
                        entries.push(pointer(TAG_INTEROP_IFD, ifds.len() - 1));
                    }
                }
            }
            let pushed = if let Some(m) = maker_note(&tiff, &exif_ifd) {
                // The source's own tag wins, for the same reason it does for
                // IPTC: it is the one the rest of this block was written
                // alongside, and two `MakerNote`s cannot both be tag `0x927C`.
                if companion.is_some() {
                    graft = MakerNoteGraft::HostHasOwn;
                }
                entries.push(m);
                true
            } else if let Some(c) = companion {
                match graft_maker_note(c, tiff.little_endian) {
                    Ok(entry) => {
                        entries.push(entry);
                        maker_is_graft = true;
                        graft = MakerNoteGraft::Carried { bytes: c.len() };
                        true
                    }
                    Err(why) => {
                        graft = why;
                        false
                    }
                }
            } else {
                false
            };
            // Only a *pinned* entry is a candidate for withdrawal, and only a
            // `MakerNote` over four bytes long gets pinned — a shorter one lives
            // inside its entry, where there is no offset to be unreachable.
            let withdrawable =
                pushed && matches!(entries.last().map(|e| &e.value), Some(Value::Pinned { .. }));
            if !entries.is_empty() {
                ifds.push(PlannedIfd {
                    entries,
                    next: None,
                });
                let idx = ifds.len() - 1;
                if withdrawable {
                    maker_at = Some((idx, ifds[idx].entries.len() - 1));
                }
                root.push(pointer(TAG_EXIF_IFD, idx));
            }
        }
    }

    if let Some(off) = sub_ifd_offset(&tiff, &ifd0, TAG_GPS_IFD) {
        if let Ok(gps) = tiff.read_ifd(off) {
            let kept = plan(&tiff, &gps, |_| true)?;
            if !kept.is_empty() {
                ifds.push(PlannedIfd {
                    entries: kept,
                    next: None,
                });
                root.push(pointer(TAG_GPS_IFD, ifds.len() - 1));
            }
        }
    }

    let mut thumb_idx = None;
    if let Some(entries) = plan_thumbnail(&tiff, &ifd0) {
        ifds.push(PlannedIfd {
            entries,
            next: None,
        });
        thumb_idx = Some(ifds.len() - 1);
    }

    ifds[0].entries = root;
    ifds[0].next = thumb_idx;

    // A `MakerNote` can only be pinned to an offset the IFDs have not already
    // claimed. Both terms are known now and neither depends on the value area,
    // so this is decidable before a byte is written.
    let mut dropped_maker_note = false;
    if let Some((i, k)) = maker_at {
        let region_end = ifd_region_end(&ifds);
        let Value::Pinned { offset, .. } = ifds[i].entries[k].value else {
            unreachable!("maker_at names a pinned entry")
        };
        if offset < region_end || offset - region_end > MAX_PIN_PADDING {
            ifds[i].entries.remove(k);
            if maker_is_graft {
                graft = MakerNoteGraft::Unreachable;
            } else {
                dropped_maker_note = true;
            }
            // An IFD holding nothing is worse than no IFD: drop the pointer to
            // it too, so the block never names an empty sub-IFD.
            if ifds[i].entries.is_empty() {
                ifds[0]
                    .entries
                    .retain(|e| !matches!(e.value, Value::IfdPointer(t) if t == i));
            }
        }
    }
    // A graft that never reached a decision had nowhere to go: this block states
    // no Exif IFD, or the one it states could not be read.
    if companion.is_some() && graft == MakerNoteGraft::NotOffered {
        graft = MakerNoteGraft::NoExifIfd;
    }

    let tag_count = ifds
        .iter()
        .flat_map(|i| &i.entries)
        .filter(|e| !matches!(e.value, Value::IfdPointer(_)))
        .count();
    if tag_count == 0 {
        return Ok(None);
    }
    let iptc = ifd0
        .get(TAG_IPTC)
        .and_then(|e| tiff.bytes_of(&e).ok())
        .or(external_iptc)
        .map(<[u8]>::to_vec)
        .filter(|b| !b.is_empty());
    Ok(Some(Rebuilt {
        tiff: serialize(&ifds, tiff.little_endian),
        tag_count,
        iptc,
        orientation,
        dropped_maker_note,
        maker_note_graft: graft,
    }))
}

/// First offset past the header and every IFD, which is where the value area
/// can start.
fn ifd_region_end(ifds: &[PlannedIfd]) -> usize {
    8 + ifds
        .iter()
        .map(|i| 2 + 12 * i.entries.len() + 4)
        .sum::<usize>()
}

/// The `MakerNote` as a pinned entry, or `None` when the source has none or it
/// is small enough to live inside its entry, where relocation is a non-issue.
fn maker_note<'a>(tiff: &Tiff<'a>, exif_ifd: &Ifd) -> Option<PlanEntry<'a>> {
    let e = exif_ifd.get(TAG_MAKER_NOTE)?;
    if type_size(e.typ) == 0 {
        return None;
    }
    let bytes = tiff.bytes_of(&e).ok()?;
    Some(PlanEntry {
        tag: TAG_MAKER_NOTE,
        typ: e.typ,
        count: e.count,
        value: if bytes.len() > 4 {
            Value::Pinned {
                bytes,
                offset: e.value_off,
            }
        } else {
            Value::Bytes(bytes)
        },
    })
}

/// A companion file's `MakerNote` as an entry for *this* block, or the reason it
/// cannot be one.
///
/// The blob is not touched. Every offset inside a vendor `MakerNote` is written
/// against the position it occupied in the file it came from, so the entry claims
/// that same position here and [`serialize`] pads the block out to reach it. That
/// costs the padding — 5,222 bytes for a Sony ARW — and buys a block where the
/// vendor's own pointers are correct by construction rather than by our arithmetic
/// being right about a layout no vendor documents.
fn graft_maker_note(
    c: &ForeignMakerNote,
    little_endian: bool,
) -> core::result::Result<PlanEntry<'_>, MakerNoteGraft> {
    if c.little_endian != little_endian {
        return Err(MakerNoteGraft::ByteOrderDiffers);
    }
    Ok(PlanEntry {
        tag: TAG_MAKER_NOTE,
        typ: c.typ,
        count: c.count,
        value: if c.bytes.len() > 4 {
            Value::Pinned {
                bytes: &c.bytes,
                offset: c.offset,
            }
        } else {
            Value::Bytes(&c.bytes)
        },
    })
}

/// `IFD1` and its thumbnail, when the source has one stored the way Exif
/// specifies: a self-contained JPEG named by `0x0201`/`0x0202`.
///
/// A strip-organized thumbnail is skipped rather than relocated. Carrying one
/// would mean carrying `IFD1`'s pixel-layout tags — the exact class of claim
/// [`IFD0_DROP`] removes — and no source this project reads writes one.
fn plan_thumbnail<'a>(tiff: &Tiff<'a>, ifd0: &Ifd) -> Option<Vec<PlanEntry<'a>>> {
    // A chain that ends, or points back at itself, stops here.
    if ifd0.next == 0 || ifd0.next == tiff.first_ifd {
        return None;
    }
    let ifd1 = tiff.read_ifd(ifd0.next).ok()?;
    let off = tiff.integers(&ifd1.get(TAG_THUMBNAIL_OFFSET)?).ok()?;
    let len = tiff.integers(&ifd1.get(TAG_THUMBNAIL_LENGTH)?).ok()?;
    let (&off, &len) = (off.first()?, len.first()?);
    let thumb = tiff
        .bytes
        .get(off as usize..(off as usize).checked_add(len as usize)?)
        .filter(|t| !t.is_empty())?;

    // `IFD1`'s remaining tags describe the thumbnail, and the thumbnail is
    // carried whole, so unlike `IFD0`'s they stay true — `Compression` included,
    // which is the tag that says these bytes are a JPEG.
    let mut entries = plan(tiff, &ifd1, |t| {
        !matches!(
            t,
            TAG_THUMBNAIL_OFFSET | TAG_THUMBNAIL_LENGTH | 273 | 279 | 324 | 325 | 330
        )
    })
    .ok()?;
    entries.push(PlanEntry {
        tag: TAG_THUMBNAIL_OFFSET,
        typ: 4,
        count: 1,
        value: Value::DataOffset(thumb),
    });
    let n = thumb.len() as u32;
    entries.push(PlanEntry {
        tag: TAG_THUMBNAIL_LENGTH,
        typ: 4,
        count: 1,
        value: Value::Immediate(if tiff.little_endian {
            n.to_le_bytes()
        } else {
            n.to_be_bytes()
        }),
    });
    Some(entries)
}

/// Copy the entries of one IFD that `keep` accepts.
fn plan<'a>(tiff: &Tiff<'a>, ifd: &Ifd, keep: impl Fn(u16) -> bool) -> Result<Vec<PlanEntry<'a>>> {
    let mut out = Vec::new();
    for (tag, entry) in ifd.entries() {
        if !keep(*tag) {
            continue;
        }
        // An unrecognized type has no known element size, so neither its length
        // nor its meaning can be established. Dropping it is the only honest
        // option; guessing one byte per element would emit a truncated value.
        if type_size(entry.typ) == 0 {
            continue;
        }
        let Ok(bytes) = tiff.bytes_of(entry) else {
            continue;
        };
        out.push(PlanEntry {
            tag: *tag,
            typ: entry.typ,
            count: entry.count,
            value: Value::Bytes(bytes),
        });
    }
    Ok(out)
}

/// Emit the header, the IFDs in order, then one shared value area.
fn serialize(ifds: &[PlannedIfd], little_endian: bool) -> Vec<u8> {
    // Header is 8 bytes, then every IFD back to back, then values.
    let mut ifd_offsets = Vec::with_capacity(ifds.len());
    let mut at = 8usize;
    for ifd in ifds {
        ifd_offsets.push(at);
        at += 2 + 12 * ifd.entries.len() + 4;
    }
    debug_assert_eq!(at, ifd_region_end(ifds));

    // Where every out-of-line value lands. Pinned values are placed first, in
    // ascending offset order, because each fixes an absolute position while
    // padding can only ever move the cursor forward.
    let mut placement: Vec<Vec<usize>> = ifds.iter().map(|i| vec![0; i.entries.len()]).collect();
    let mut pinned: Vec<(usize, usize, usize)> = Vec::new();
    for (i, ifd) in ifds.iter().enumerate() {
        for (k, e) in ifd.entries.iter().enumerate() {
            if let Value::Pinned { offset, .. } = e.value {
                pinned.push((offset, i, k));
            }
        }
    }
    pinned.sort_unstable();
    for (offset, i, k) in pinned {
        // `rebuild` refuses a pin the IFD region would overlap, and there is at
        // most one pinned value, so the cursor is always behind it.
        debug_assert!(offset >= at);
        placement[i][k] = offset;
        let len = match ifds[i].entries[k].value {
            Value::Pinned { bytes, .. } => bytes.len(),
            _ => 0,
        };
        at = offset + len + len % 2;
    }

    // TIFF requires out-of-line values at even offsets, and every value is
    // copied whole, so the only padding is the alignment byte between
    // odd-length neighbours.
    for (i, ifd) in ifds.iter().enumerate() {
        for (k, e) in ifd.entries.iter().enumerate() {
            let len = match &e.value {
                Value::Bytes(b) if b.len() > 4 => b.len(),
                Value::DataOffset(b) => b.len(),
                _ => continue,
            };
            placement[i][k] = at;
            at += len + len % 2;
        }
    }

    let mut w = Writer {
        buf: Vec::with_capacity(at),
        little_endian,
    };
    w.buf
        .extend_from_slice(if little_endian { b"II" } else { b"MM" });
    w.u16(42);
    w.u32(8);

    for (i, ifd) in ifds.iter().enumerate() {
        // TIFF requires entries in ascending tag order; the pointer and
        // thumbnail entries were appended after planning, so this is where that
        // is restored.
        let mut order: Vec<usize> = (0..ifd.entries.len()).collect();
        order.sort_by_key(|&k| ifd.entries[k].tag);

        w.u16(ifd.entries.len() as u16);
        for &k in &order {
            let e = &ifd.entries[k];
            w.u16(e.tag);
            w.u16(e.typ);
            w.u32(e.count);
            match &e.value {
                Value::IfdPointer(target) => w.u32(ifd_offsets[*target] as u32),
                Value::Pinned { .. } | Value::DataOffset(_) => w.u32(placement[i][k] as u32),
                Value::Immediate(b) => w.buf.extend_from_slice(b),
                Value::Bytes(b) if b.len() > 4 => w.u32(placement[i][k] as u32),
                Value::Bytes(b) => {
                    // Four bytes or fewer are stored in the entry itself,
                    // left-aligned and zero-filled to the full four.
                    w.buf.extend_from_slice(b);
                    w.buf.extend(std::iter::repeat_n(0u8, 4 - b.len()));
                }
            }
        }
        w.u32(ifd.next.map_or(0, |t| ifd_offsets[t] as u32));
    }

    // Written in offset order, so a pinned value's padding is just the gap
    // before it wherever in the plan that value happened to appear.
    let mut blobs: Vec<(usize, &[u8])> = Vec::new();
    for (i, ifd) in ifds.iter().enumerate() {
        for (k, e) in ifd.entries.iter().enumerate() {
            let b = match &e.value {
                Value::Pinned { bytes, .. } => *bytes,
                Value::DataOffset(b) => b,
                Value::Bytes(b) if b.len() > 4 => b,
                _ => continue,
            };
            blobs.push((placement[i][k], b));
        }
    }
    blobs.sort_by_key(|(o, _)| *o);
    for (off, b) in blobs {
        debug_assert!(w.buf.len() <= off);
        w.buf.resize(off, 0);
        w.buf.extend_from_slice(b);
        if b.len() % 2 == 1 {
            w.buf.push(0);
        }
    }
    w.buf
}

struct Writer {
    buf: Vec<u8>,
    little_endian: bool,
}

impl Writer {
    fn u16(&mut self, v: u16) {
        self.buf.extend_from_slice(&if self.little_endian {
            v.to_le_bytes()
        } else {
            v.to_be_bytes()
        });
    }

    fn u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&if self.little_endian {
            v.to_le_bytes()
        } else {
            v.to_be_bytes()
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a TIFF by hand so the tests state exactly what the reader sees.
    struct Build {
        le: bool,
        entries: Vec<(u16, u16, u32, Vec<u8>)>,
        subs: Vec<(u16, Vec<(u16, u16, u32, Vec<u8>)>)>,
        /// `IFD1`'s entries, plus the thumbnail bytes tag 513 must point at.
        ifd1: Option<(Vec<(u16, u16, u32, Vec<u8>)>, Vec<u8>)>,
    }

    impl Build {
        fn new(le: bool) -> Self {
            Build {
                le,
                entries: Vec::new(),
                subs: Vec::new(),
                ifd1: None,
            }
        }

        fn ascii(mut self, tag: u16, s: &str) -> Self {
            let mut v = s.as_bytes().to_vec();
            v.push(0);
            let n = v.len() as u32;
            self.entries.push((tag, 2, n, v));
            self
        }

        fn short(mut self, tag: u16, v: u16) -> Self {
            let b = if self.le {
                v.to_le_bytes()
            } else {
                v.to_be_bytes()
            };
            self.entries.push((tag, 3, 1, b.to_vec()));
            self
        }

        fn long(mut self, tag: u16, v: u32) -> Self {
            let b = if self.le {
                v.to_le_bytes()
            } else {
                v.to_be_bytes()
            };
            self.entries.push((tag, 4, 1, b.to_vec()));
            self
        }

        fn undefined(mut self, tag: u16, v: &[u8]) -> Self {
            self.entries.push((tag, 7, v.len() as u32, v.to_vec()));
            self
        }

        /// A tag whose type this reader has no size for.
        fn bogus_type(mut self, tag: u16) -> Self {
            self.entries.push((tag, 999, 1, vec![0, 0, 0, 0]));
            self
        }

        fn sub(mut self, ptr_tag: u16, entries: Vec<(u16, u16, u32, Vec<u8>)>) -> Self {
            self.subs.push((ptr_tag, entries));
            self
        }

        fn thumbnail(mut self, jpeg: &[u8]) -> Self {
            self.ifd1 = Some((
                vec![
                    (259, 3, 1, vec![6, 0]), // Compression: JPEG
                    (513, 4, 1, vec![0, 0, 0, 0]),
                    (514, 4, 1, vec![0, 0, 0, 0]),
                ],
                jpeg.to_vec(),
            ));
            self
        }

        fn build(&self) -> Vec<u8> {
            let u16b = |v: u16| {
                if self.le {
                    v.to_le_bytes()
                } else {
                    v.to_be_bytes()
                }
            };
            let u32b = |v: u32| {
                if self.le {
                    v.to_le_bytes()
                } else {
                    v.to_be_bytes()
                }
            };

            let n_root = self.entries.len() + self.subs.len();
            let mut at = 8 + 2 + 12 * n_root + 4;
            let mut sub_at = Vec::new();
            for (_, e) in &self.subs {
                sub_at.push(at);
                at += 2 + 12 * e.len() + 4;
            }
            let ifd1_at = self.ifd1.as_ref().map(|(e, _)| {
                let a = at;
                at += 2 + 12 * e.len() + 4;
                a
            });

            // Lay out the out-of-line values after every IFD.
            let mut vals: Vec<u8> = Vec::new();
            let place = |b: &Vec<u8>, at: usize, vals: &mut Vec<u8>| -> [u8; 4] {
                if b.len() <= 4 {
                    let mut p = [0u8; 4];
                    p[..b.len()].copy_from_slice(b);
                    p
                } else {
                    let off = at + vals.len();
                    vals.extend_from_slice(b);
                    if b.len() % 2 == 1 {
                        vals.push(0);
                    }
                    u32b(off as u32)
                }
            };

            let mut root = Vec::new();
            let mut sorted = self.entries.clone();
            for (i, (ptr, _)) in self.subs.iter().enumerate() {
                sorted.push((*ptr, 4, 1, u32b(sub_at[i] as u32).to_vec()));
            }
            sorted.sort_by_key(|(t, ..)| *t);
            root.extend_from_slice(&u16b(sorted.len() as u16));
            for (tag, typ, count, b) in &sorted {
                root.extend_from_slice(&u16b(*tag));
                root.extend_from_slice(&u16b(*typ));
                root.extend_from_slice(&u32b(*count));
                root.extend_from_slice(&place(b, at, &mut vals));
            }
            root.extend_from_slice(&u32b(ifd1_at.unwrap_or(0) as u32));

            let mut subs_buf = Vec::new();
            for (_, entries) in &self.subs {
                let mut e2 = entries.clone();
                e2.sort_by_key(|(t, ..)| *t);
                subs_buf.extend_from_slice(&u16b(e2.len() as u16));
                for (tag, typ, count, b) in &e2 {
                    subs_buf.extend_from_slice(&u16b(*tag));
                    subs_buf.extend_from_slice(&u16b(*typ));
                    subs_buf.extend_from_slice(&u32b(*count));
                    subs_buf.extend_from_slice(&place(b, at, &mut vals));
                }
                subs_buf.extend_from_slice(&u32b(0));
            }

            let mut ifd1_buf = Vec::new();
            if let Some((entries, jpeg)) = &self.ifd1 {
                let thumb_off = at + vals.len();
                vals.extend_from_slice(jpeg);
                if jpeg.len() % 2 == 1 {
                    vals.push(0);
                }
                ifd1_buf.extend_from_slice(&u16b(entries.len() as u16));
                for (tag, typ, count, b) in entries {
                    ifd1_buf.extend_from_slice(&u16b(*tag));
                    ifd1_buf.extend_from_slice(&u16b(*typ));
                    ifd1_buf.extend_from_slice(&u32b(*count));
                    match *tag {
                        513 => ifd1_buf.extend_from_slice(&u32b(thumb_off as u32)),
                        514 => ifd1_buf.extend_from_slice(&u32b(jpeg.len() as u32)),
                        _ => ifd1_buf.extend_from_slice(&place(b, at, &mut vals)),
                    }
                }
                ifd1_buf.extend_from_slice(&u32b(0));
            }

            let mut out = Vec::new();
            out.extend_from_slice(if self.le { b"II" } else { b"MM" });
            out.extend_from_slice(&u16b(42));
            out.extend_from_slice(&u32b(8));
            out.extend_from_slice(&root);
            out.extend_from_slice(&subs_buf);
            out.extend_from_slice(&ifd1_buf);
            out.extend_from_slice(&vals);
            out
        }
    }

    fn exif_ifd() -> Vec<(u16, u16, u32, Vec<u8>)> {
        vec![
            // DateTimeOriginal, an out-of-line ASCII value.
            (36867, 2, 20, b"2026:06:13 08:29:00\0".to_vec()),
            // ExposureTime, a RATIONAL — two LONGs, byte order matters.
            (33434, 5, 1, vec![1, 0, 0, 0, 100, 0, 0, 0]),
        ]
    }

    /// Reading back a rebuilt block must find the same tags, which is the
    /// property the whole module exists to provide.
    #[test]
    fn round_trips_through_its_own_reader() {
        let src = Build::new(true)
            .ascii(271, "Apple")
            .ascii(272, "iPhone 17 Pro")
            .short(274, 6)
            .sub(TAG_EXIF_IFD, exif_ifd())
            .build();

        let first = read_bytes(&src).unwrap().expect("source has exif");
        assert_eq!(first.origin, Origin::Tiff);

        // Feed the rebuilt block back in as a JPEG, so the second parse uses a
        // different locate() arm than the first.
        let carrier = tohdr_core::exif::wrap_in_jpeg(&first.tiff).expect("fits one APP1");
        let again = read_bytes(&carrier)
            .unwrap()
            .expect("rebuilt block still has exif");
        assert_eq!(again.origin, Origin::Jpeg);
        assert_eq!(again.tag_count, first.tag_count);
        // Idempotent: rebuilding an already-rebuilt block changes nothing.
        assert_eq!(again.tiff, first.tiff);
    }

    #[test]
    fn keeps_make_model_and_the_exif_ifd() {
        let src = Build::new(true)
            .ascii(271, "Apple")
            .ascii(272, "iPhone 17 Pro")
            .sub(TAG_EXIF_IFD, exif_ifd())
            .build();
        let out = read_bytes(&src).unwrap().unwrap();

        let tiff = Tiff::open(&out.tiff).unwrap().unwrap();
        let ifd0 = tiff.read_ifd(tiff.first_ifd).unwrap();
        let make = tiff.bytes_of(&ifd0.get(271).unwrap()).unwrap();
        assert_eq!(make, b"Apple\0");
        let model = tiff.bytes_of(&ifd0.get(272).unwrap()).unwrap();
        assert_eq!(model, b"iPhone 17 Pro\0");

        let sub_off = tiff.integers(&ifd0.get(TAG_EXIF_IFD).unwrap()).unwrap()[0];
        let sub = tiff.read_ifd(sub_off as usize).unwrap();
        let dto = tiff.bytes_of(&sub.get(36867).unwrap()).unwrap();
        assert_eq!(dto, b"2026:06:13 08:29:00\0");
        // Make, Model, DateTimeOriginal, ExposureTime. The pointer entry is not
        // counted.
        assert_eq!(out.tag_count, 4);
    }

    /// A RATIONAL is two LONGs, so a byte-order bug shows up here and nowhere
    /// else — and emitting in the source's order is what makes it a copy.
    #[test]
    fn big_endian_rationals_survive() {
        let src = Build::new(false)
            .ascii(271, "Nikon")
            .sub(
                TAG_EXIF_IFD,
                vec![(33434, 5, 1, vec![0, 0, 0, 1, 0, 0, 0, 100])],
            )
            .build();
        let out = read_bytes(&src).unwrap().unwrap();
        assert_eq!(&out.tiff[0..2], b"MM");

        let tiff = Tiff::open(&out.tiff).unwrap().unwrap();
        let ifd0 = tiff.read_ifd(tiff.first_ifd).unwrap();
        let sub_off = tiff.integers(&ifd0.get(TAG_EXIF_IFD).unwrap()).unwrap()[0];
        let sub = tiff.read_ifd(sub_off as usize).unwrap();
        let rational = tiff.bytes_of(&sub.get(33434).unwrap()).unwrap();
        assert_eq!(rational, &[0, 0, 0, 1, 0, 0, 0, 100]);
    }

    /// The three classes of [`IFD0_DROP`], one tag each.
    #[test]
    fn drops_pixel_layout_and_dangling_offsets() {
        let src = Build::new(true)
            .ascii(271, "Apple")
            .short(259, 1) // Compression — pixel layout
            .short(262, 2) // PhotometricInterpretation
            .long(273, 4096) // StripOffsets — a live pointer into nothing
            .short(330, 9) // SubIFDs — the Lightroom gain map
            .undefined(700, b"<x:xmpmeta/>") // XMP — carried as its own item
            .sub(TAG_EXIF_IFD, exif_ifd())
            .build();
        let out = read_bytes(&src).unwrap().unwrap();

        let tiff = Tiff::open(&out.tiff).unwrap().unwrap();
        let ifd0 = tiff.read_ifd(tiff.first_ifd).unwrap();
        for tag in [259u16, 262, 273, 330, 700] {
            assert!(ifd0.get(tag).is_none(), "tag {tag} should be dropped");
        }
        assert!(ifd0.get(271).is_some(), "Make should survive");
    }

    /// A tag with no name in this module is carried, which is the whole point of
    /// a denylist: the tags we lose should be the ones we chose to lose.
    #[test]
    fn tags_this_module_has_never_heard_of_are_carried() {
        let src = Build::new(true)
            .ascii(271, "Apple")
            .ascii(42016, "unique-image-id") // ImageUniqueID, never named here
            .undefined(33723, b"iptc-iim-block") // IPTC-NAA
            .short(4242, 7) // and one that is not a real tag at all
            .build();
        let out = read_bytes(&src).unwrap().unwrap();
        let tiff = Tiff::open(&out.tiff).unwrap().unwrap();
        let ifd0 = tiff.read_ifd(tiff.first_ifd).unwrap();
        assert_eq!(
            tiff.bytes_of(&ifd0.get(42016).unwrap()).unwrap(),
            b"unique-image-id\0"
        );
        assert_eq!(
            tiff.bytes_of(&ifd0.get(33723).unwrap()).unwrap(),
            b"iptc-iim-block"
        );
        assert!(ifd0.get(4242).is_some());
    }

    /// The property that makes a relocated `MakerNote` still parse: its bytes
    /// end up at the same block-relative offset they were written for.
    #[test]
    fn the_maker_note_keeps_its_original_offset() {
        // Long enough that the pin offset lands well past the output's IFDs.
        let note = b"Apple iOS\0\0\x01MM".repeat(40);
        let src = Build::new(true)
            .ascii(271, "Apple")
            .sub(
                TAG_EXIF_IFD,
                vec![
                    (36867, 2, 20, b"2026:06:13 08:29:00\0".to_vec()),
                    (TAG_MAKER_NOTE, 7, note.len() as u32, note.clone()),
                ],
            )
            .build();

        // Where the source put it, read straight out of the source.
        let src_tiff = Tiff::open(&src).unwrap().unwrap();
        let src_ifd0 = src_tiff.read_ifd(src_tiff.first_ifd).unwrap();
        let src_sub = src_tiff
            .read_ifd(
                src_tiff
                    .integers(&src_ifd0.get(TAG_EXIF_IFD).unwrap())
                    .unwrap()[0] as usize,
            )
            .unwrap();
        let want_off = src_sub.get(TAG_MAKER_NOTE).unwrap().value_off;

        let out = read_bytes(&src).unwrap().unwrap();
        assert!(!out.dropped_maker_note);
        let tiff = Tiff::open(&out.tiff).unwrap().unwrap();
        let ifd0 = tiff.read_ifd(tiff.first_ifd).unwrap();
        let sub = tiff
            .read_ifd(tiff.integers(&ifd0.get(TAG_EXIF_IFD).unwrap()).unwrap()[0] as usize)
            .unwrap();
        let got = sub.get(TAG_MAKER_NOTE).unwrap();
        assert_eq!(got.value_off, want_off, "MakerNote moved");
        assert_eq!(tiff.bytes_of(&got).unwrap(), &note[..]);
    }

    /// A pin the output's own IFDs would sit on top of cannot be honored, and
    /// then dropping the tag is the only choice that leaves a parseable block.
    ///
    /// Reaching that state takes a source laid out backwards — values first,
    /// IFDs after — because a conventional one puts its values past its IFDs and
    /// this module only ever *removes* entries, so its IFD region cannot grow
    /// past where the source's values began.
    #[test]
    fn an_unreachable_maker_note_offset_is_reported_not_forced() {
        let mut src = vec![0u8; 320];
        let put16 = |s: &mut Vec<u8>, at: usize, v: u16| {
            s[at..at + 2].copy_from_slice(&v.to_le_bytes())
        };
        let put32 = |s: &mut Vec<u8>, at: usize, v: u32| {
            s[at..at + 4].copy_from_slice(&v.to_le_bytes())
        };
        src[0..2].copy_from_slice(b"II");
        put16(&mut src, 2, 42);
        put32(&mut src, 4, 200); // IFD0 lives at the far end
        src[20..28].copy_from_slice(b"Apple\0\0\0"); // the MakerNote value, at 20
        src[30..36].copy_from_slice(b"Apple\0"); // Make's value
        put16(&mut src, 200, 2);
        put16(&mut src, 202, 271); // Make
        put16(&mut src, 204, 2);
        put32(&mut src, 206, 6);
        put32(&mut src, 210, 30);
        put16(&mut src, 214, TAG_EXIF_IFD);
        put16(&mut src, 216, 4);
        put32(&mut src, 218, 1);
        put32(&mut src, 222, 300);
        put16(&mut src, 300, 1);
        put16(&mut src, 302, TAG_MAKER_NOTE);
        put16(&mut src, 304, 7);
        put32(&mut src, 306, 8);
        put32(&mut src, 310, 20);

        let out = read_bytes(&src).unwrap().unwrap();
        assert!(out.dropped_maker_note);
        let tiff = Tiff::open(&out.tiff).unwrap().unwrap();
        let ifd0 = tiff.read_ifd(tiff.first_ifd).unwrap();
        // The Exif IFD held nothing else, so its pointer goes with it rather
        // than naming an empty IFD.
        assert!(ifd0.get(TAG_EXIF_IFD).is_none());
        assert!(ifd0.get(271).is_some());
    }

    // ===================================================================
    // Grafting a `MakerNote` out of the original camera file
    // ===================================================================

    /// A camera file: an Exif IFD whose only interesting tag is the `MakerNote`,
    /// plus enough around it to be a real block.
    fn raw_with_maker_note(le: bool, note: &[u8]) -> Vec<u8> {
        Build::new(le)
            .ascii(271, "SONY")
            .ascii(272, "ILCE-7RM5")
            .sub(
                TAG_EXIF_IFD,
                vec![
                    (36867, 2, 20, b"2026:06:13 08:29:00\0".to_vec()),
                    (TAG_MAKER_NOTE, 7, note.len() as u32, note.to_vec()),
                ],
            )
            .build()
    }

    /// A rendered intermediate: what Lightroom hands us, which is to say the same
    /// photograph's Exif with the vendor block gone.
    fn rendered_without_maker_note() -> Vec<u8> {
        Build::new(true)
            .ascii(271, "SONY")
            .ascii(272, "ILCE-7RM5")
            .ascii(305, "Adobe Photoshop Lightroom Classic 15.3")
            .short(274, 1)
            .sub(TAG_EXIF_IFD, exif_ifd())
            .build()
    }

    fn maker_note_of(block: &[u8]) -> Option<(crate::gainmap_tiff::Entry, Vec<u8>)> {
        let tiff = Tiff::open(block).unwrap()?;
        let ifd0 = tiff.read_ifd(tiff.first_ifd).unwrap();
        let sub = tiff
            .read_ifd(tiff.integers(&ifd0.get(TAG_EXIF_IFD)?).unwrap()[0] as usize)
            .unwrap();
        let e = sub.get(TAG_MAKER_NOTE)?;
        Some((e, tiff.bytes_of(&e).unwrap().to_vec()))
    }

    /// The whole point: the blob lands at the offset it had in the raw, byte for
    /// byte, so the file-absolute pointers inside it address the same bytes they
    /// were written to address. Nothing is rebased, because nothing has to be.
    #[test]
    fn a_companion_maker_note_lands_at_the_offset_it_came_from() {
        // Long enough to be pinned rather than stored inside its entry.
        let note = b"\x72\x00sony-vendor-block".repeat(8);
        let raw = raw_with_maker_note(true, &note);
        let companion = maker_note_from_bytes(&raw).expect("the raw has one");
        assert_eq!(companion.len(), note.len());
        assert_eq!(
            companion.offset(),
            maker_note_of(&raw).unwrap().0.value_off,
            "the offset carried is the one the raw used"
        );

        let out = read_bytes_with_maker_note(&rendered_without_maker_note(), Some(&companion))
            .unwrap()
            .unwrap();
        assert_eq!(
            out.maker_note_graft,
            MakerNoteGraft::Carried { bytes: note.len() }
        );
        let (entry, bytes) = maker_note_of(&out.tiff).expect("grafted");
        assert_eq!(entry.value_off, companion.offset(), "the blob moved");
        assert_eq!(bytes, note, "the blob was altered");
        assert_eq!(entry.typ, 7, "the raw's own type claim is kept");

        // And the tags that were already there are still there, so the graft
        // added rather than replaced.
        let tiff = Tiff::open(&out.tiff).unwrap().unwrap();
        let ifd0 = tiff.read_ifd(tiff.first_ifd).unwrap();
        assert_eq!(tiff.bytes_of(&ifd0.get(271).unwrap()).unwrap(), b"SONY\0");
        assert_eq!(
            tiff.bytes_of(&ifd0.get(305).unwrap()).unwrap(),
            b"Adobe Photoshop Lightroom Classic 15.3\0"
        );
    }

    /// A grafted block has to survive our own reader, and survive it *unmoved* —
    /// a second pass that relocated the blob would break it just as thoroughly as
    /// never pinning it in the first place.
    #[test]
    fn a_grafted_block_rebuilds_to_itself() {
        let note = b"\x72\x00sony-vendor-block".repeat(8);
        let companion = maker_note_from_bytes(&raw_with_maker_note(true, &note)).unwrap();
        let first = read_bytes_with_maker_note(&rendered_without_maker_note(), Some(&companion))
            .unwrap()
            .unwrap();

        let again = read_bytes(&first.tiff).unwrap().unwrap();
        assert_eq!(again.tiff, first.tiff, "rebuilding it again changed it");
        // Now it is the block's *own* MakerNote, so that is what is reported.
        assert_eq!(again.maker_note_graft, MakerNoteGraft::NotOffered);
        assert!(!again.dropped_maker_note);
        assert_eq!(maker_note_of(&again.tiff).unwrap().1, note);
    }

    /// Offered one when the block already has its own: the block's own wins. Its
    /// neighbours were written alongside *that* one, and tag `0x927C` can only be
    /// there once.
    #[test]
    fn the_blocks_own_maker_note_beats_a_companions() {
        let mine = b"\x72\x00my-own-vendor-block".repeat(8);
        let theirs = b"\x72\x00someone-elses-block".repeat(8);
        let host = raw_with_maker_note(true, &mine);
        let companion = maker_note_from_bytes(&raw_with_maker_note(true, &theirs)).unwrap();

        let out = read_bytes_with_maker_note(&host, Some(&companion))
            .unwrap()
            .unwrap();
        assert_eq!(out.maker_note_graft, MakerNoteGraft::HostHasOwn);
        assert_eq!(maker_note_of(&out.tiff).unwrap().1, mine);
        // Byte-identical to what no companion at all would have produced.
        assert_eq!(out.tiff, read_bytes(&host).unwrap().unwrap().tiff);
    }

    /// Sony's `MakerNote` opens with its entry count and carries no byte-order
    /// mark, so a reader takes the order from the enclosing block. Grafted across
    /// a byte-order boundary it would parse cleanly and report transposed
    /// nonsense — the one failure mode worse than losing the tag.
    #[test]
    fn a_companion_of_the_other_byte_order_is_refused() {
        let note = b"\x00\x72sony-vendor-block".repeat(8);
        let companion = maker_note_from_bytes(&raw_with_maker_note(false, &note)).unwrap();
        let host = rendered_without_maker_note(); // little-endian

        let out = read_bytes_with_maker_note(&host, Some(&companion))
            .unwrap()
            .unwrap();
        assert_eq!(out.maker_note_graft, MakerNoteGraft::ByteOrderDiffers);
        assert!(maker_note_of(&out.tiff).is_none());
        assert_eq!(out.tiff, read_bytes(&host).unwrap().unwrap().tiff);
    }

    /// Nowhere to put it: a `MakerNote` belongs in the Exif IFD, and inventing
    /// one to hold a foreign tag would add a sub-IFD the source never had.
    #[test]
    fn a_block_with_no_exif_ifd_takes_no_graft() {
        let note = b"\x72\x00sony-vendor-block".repeat(8);
        let companion = maker_note_from_bytes(&raw_with_maker_note(true, &note)).unwrap();
        let host = Build::new(true).ascii(271, "SONY").short(274, 1).build();

        let out = read_bytes_with_maker_note(&host, Some(&companion))
            .unwrap()
            .unwrap();
        assert_eq!(out.maker_note_graft, MakerNoteGraft::NoExifIfd);
        assert_eq!(out.tiff, read_bytes(&host).unwrap().unwrap().tiff);
    }

    /// A companion whose offset the host's own IFDs already occupy. Padding only
    /// ever moves the cursor forward, so this one cannot be honored — and a graft
    /// withdrawn is reported as such rather than as the host having dropped a
    /// `MakerNote` it never had.
    #[test]
    fn a_companion_offset_the_ifds_already_claim_is_refused() {
        // Nothing but the Exif IFD, so the blob sits at a very low offset.
        let raw = Build::new(true)
            .sub(TAG_EXIF_IFD, vec![(TAG_MAKER_NOTE, 7, 8, b"vendor!!".to_vec())])
            .build();
        let companion = maker_note_from_bytes(&raw).unwrap();
        let host = rendered_without_maker_note();
        assert!(
            companion.offset() < 8 + 2 + 12 * 5 + 4,
            "the fixture must collide with the host's IFD0 alone, got {}",
            companion.offset()
        );

        let out = read_bytes_with_maker_note(&host, Some(&companion))
            .unwrap()
            .unwrap();
        assert_eq!(out.maker_note_graft, MakerNoteGraft::Unreachable);
        assert!(
            !out.dropped_maker_note,
            "the host had no MakerNote of its own to drop"
        );
        assert!(maker_note_of(&out.tiff).is_none());
        // The Exif IFD keeps its other tags, so its pointer stays.
        let tiff = Tiff::open(&out.tiff).unwrap().unwrap();
        let ifd0 = tiff.read_ifd(tiff.first_ifd).unwrap();
        assert!(ifd0.get(TAG_EXIF_IFD).is_some());
    }

    /// The distinction that keeps the bounded read honest: a slice that stops
    /// short of the blob must say "not here", not "there is none". Only the first
    /// answer is worth re-reading the file for, and `read_maker_note` re-reads on
    /// exactly that one.
    #[test]
    fn a_truncated_head_is_undecided_rather_than_negative() {
        let note = b"\x72\x00sony-vendor-block".repeat(8);
        let raw = raw_with_maker_note(true, &note);
        let at = maker_note_of(&raw).unwrap().0.value_off;

        assert!(matches!(maker_note_in(&raw), Located::Found(_)));
        // Cut inside the blob: the IFDs still parse and name bytes that are gone.
        assert!(matches!(maker_note_in(&raw[..at + 4]), Located::Beyond));
        // Cut before the IFDs even resolve.
        assert!(matches!(maker_note_in(&raw[..10]), Located::Beyond));
        // A block that genuinely has none is a real answer, not a short read.
        let bare = Build::new(true).ascii(271, "SONY").build();
        assert!(matches!(maker_note_in(&bare), Located::Absent));
        let no_tag = Build::new(true).sub(TAG_EXIF_IFD, exif_ifd()).build();
        assert!(matches!(maker_note_in(&no_tag), Located::Absent));
    }

    #[test]
    fn gps_is_carried() {
        let src = Build::new(true)
            .ascii(271, "Apple")
            .sub(
                TAG_GPS_IFD,
                vec![(1, 2, 2, b"N\0".to_vec()), (5, 1, 1, vec![0, 0, 0, 0])],
            )
            .build();
        let out = read_bytes(&src).unwrap().unwrap();
        let tiff = Tiff::open(&out.tiff).unwrap().unwrap();
        let ifd0 = tiff.read_ifd(tiff.first_ifd).unwrap();
        let gps_off = tiff.integers(&ifd0.get(TAG_GPS_IFD).unwrap()).unwrap()[0];
        let gps = tiff.read_ifd(gps_off as usize).unwrap();
        assert_eq!(tiff.bytes_of(&gps.get(1).unwrap()).unwrap(), b"N\0");
    }

    /// The Interoperability IFD hangs off the *Exif* IFD, not `IFD0`, so it is
    /// the one sub-IFD reached two pointers deep.
    #[test]
    fn the_interop_ifd_is_carried() {
        // Hand-build the nesting the Build helper cannot express: IFD0 -> Exif
        // IFD -> Interop IFD.
        let mut src = vec![];
        src.extend_from_slice(b"II\x2a\x00");
        src.extend_from_slice(&8u32.to_le_bytes());
        // IFD0 at 8: one entry, the Exif pointer.
        src.extend_from_slice(&1u16.to_le_bytes());
        src.extend_from_slice(&TAG_EXIF_IFD.to_le_bytes());
        src.extend_from_slice(&4u16.to_le_bytes());
        src.extend_from_slice(&1u32.to_le_bytes());
        src.extend_from_slice(&26u32.to_le_bytes()); // Exif IFD offset
        src.extend_from_slice(&0u32.to_le_bytes());
        // Exif IFD at 26: one entry, the Interop pointer.
        src.extend_from_slice(&1u16.to_le_bytes());
        src.extend_from_slice(&TAG_INTEROP_IFD.to_le_bytes());
        src.extend_from_slice(&4u16.to_le_bytes());
        src.extend_from_slice(&1u32.to_le_bytes());
        src.extend_from_slice(&44u32.to_le_bytes()); // Interop IFD offset
        src.extend_from_slice(&0u32.to_le_bytes());
        // Interop IFD at 44: InteropIndex = "R98".
        src.extend_from_slice(&1u16.to_le_bytes());
        src.extend_from_slice(&1u16.to_le_bytes());
        src.extend_from_slice(&2u16.to_le_bytes());
        src.extend_from_slice(&4u32.to_le_bytes());
        src.extend_from_slice(b"R98\0");
        src.extend_from_slice(&0u32.to_le_bytes());

        let out = read_bytes(&src).unwrap().unwrap();
        let tiff = Tiff::open(&out.tiff).unwrap().unwrap();
        let ifd0 = tiff.read_ifd(tiff.first_ifd).unwrap();
        let exif = tiff
            .read_ifd(tiff.integers(&ifd0.get(TAG_EXIF_IFD).unwrap()).unwrap()[0] as usize)
            .unwrap();
        let interop = tiff
            .read_ifd(tiff.integers(&exif.get(TAG_INTEROP_IFD).unwrap()).unwrap()[0] as usize)
            .unwrap();
        assert_eq!(tiff.bytes_of(&interop.get(1).unwrap()).unwrap(), b"R98\0");
    }

    #[test]
    fn orientation_is_carried_and_reported() {
        for want in 1u8..=8 {
            let src = Build::new(true)
                .ascii(271, "Apple")
                .short(TAG_ORIENTATION, want as u16)
                .build();
            let out = read_bytes(&src).unwrap().unwrap();
            assert_eq!(out.orientation, want);
            let tiff = Tiff::open(&out.tiff).unwrap().unwrap();
            let ifd0 = tiff.read_ifd(tiff.first_ifd).unwrap();
            let e = ifd0.get(TAG_ORIENTATION).expect("orientation kept");
            assert_eq!(tiff.integers(&e).unwrap()[0], want as u32);
        }
    }

    /// Out of range and absent both mean "upright", so a caller never has to
    /// second-guess the number.
    #[test]
    fn a_nonsense_orientation_reads_as_upright() {
        let src = Build::new(true)
            .ascii(271, "Apple")
            .short(TAG_ORIENTATION, 99)
            .build();
        assert_eq!(read_bytes(&src).unwrap().unwrap().orientation, 1);
        let src = Build::new(true).ascii(271, "Apple").build();
        assert_eq!(read_bytes(&src).unwrap().unwrap().orientation, 1);
    }

    #[test]
    fn the_thumbnail_ifd_survives_with_its_jpeg() {
        // Odd-length on purpose: the thumbnail is the largest value in the
        // block, so anything placed after it would inherit a misalignment.
        let mut jpeg = vec![0xffu8, 0xd8];
        jpeg.extend(std::iter::repeat_n(0x41u8, 61));
        jpeg.extend_from_slice(&[0xff, 0xd9]);
        let jpeg = &jpeg[..];
        let src = Build::new(true)
            .ascii(271, "Apple")
            .sub(TAG_EXIF_IFD, exif_ifd())
            .thumbnail(jpeg)
            .build();
        let out = read_bytes(&src).unwrap().unwrap();

        let tiff = Tiff::open(&out.tiff).unwrap().unwrap();
        let ifd0 = tiff.read_ifd(tiff.first_ifd).unwrap();
        assert_ne!(ifd0.next, 0, "IFD0 must chain to IFD1");
        let ifd1 = tiff.read_ifd(ifd0.next).unwrap();
        let off = tiff
            .integers(&ifd1.get(TAG_THUMBNAIL_OFFSET).unwrap())
            .unwrap()[0] as usize;
        let len = tiff
            .integers(&ifd1.get(TAG_THUMBNAIL_LENGTH).unwrap())
            .unwrap()[0] as usize;
        assert_eq!(len, jpeg.len());
        assert_eq!(&out.tiff[off..off + len], jpeg);
        // Compression says these bytes are a JPEG, which is true of what was
        // carried, so unlike IFD0's copy it stays.
        assert_eq!(tiff.integers(&ifd1.get(259).unwrap()).unwrap()[0], 6);
    }

    /// An Apple `MakerNote`: signature, version, its own byte-order mark, then a
    /// big-endian IFD whose offsets are relative to the note's first byte.
    ///
    /// Built here rather than copied from a fixture so the offsets are the thing
    /// under test: `align_apple_headroom` has to find tag 48's value through the
    /// note's own base, not the enclosing block's.
    fn apple_maker_note(tag33: (i32, i32), tag48: (i32, i32)) -> Vec<u8> {
        let entries: [(u16, u16, u32, Option<(i32, i32)>); 4] = [
            (2, 7, 4, None),                        // an inline value, before
            (APPLE_TAG_HEADROOM, 10, 1, Some(tag33)),
            (APPLE_TAG_GAIN, 10, 1, Some(tag48)),
            (54, 7, 4, None), // and one after, so removal has to compact
        ];
        let mut mn = Vec::new();
        mn.extend_from_slice(APPLE_MAKER_SIG);
        mn.push(1);
        mn.extend_from_slice(b"MM");
        mn.extend_from_slice(&(entries.len() as u16).to_be_bytes());
        // Values live after the IFD, which is where Apple puts them.
        let mut at = APPLE_MAKER_IFD_AT + 2 + entries.len() * 12 + 4;
        let mut vals = Vec::new();
        for (tag, typ, count, r) in entries {
            mn.extend_from_slice(&tag.to_be_bytes());
            mn.extend_from_slice(&typ.to_be_bytes());
            mn.extend_from_slice(&count.to_be_bytes());
            match r {
                Some((n, d)) => {
                    mn.extend_from_slice(&(at as u32).to_be_bytes());
                    vals.extend_from_slice(&n.to_be_bytes());
                    vals.extend_from_slice(&d.to_be_bytes());
                    at += 8;
                }
                None => mn.extend_from_slice(b"XXXX"),
            }
        }
        mn.extend_from_slice(&0u32.to_be_bytes()); // next IFD
        mn.extend_from_slice(&vals);
        mn
    }

    /// The outer block is little-endian while the note is big-endian, so a
    /// reader that used the wrong one reads nonsense.
    fn block_with_apple_maker_note(mn: &[u8]) -> Vec<u8> {
        Build::new(true)
            .ascii(271, "Apple")
            .sub(
                TAG_EXIF_IFD,
                vec![(TAG_MAKER_NOTE, 7, mn.len() as u32, mn.to_vec())],
            )
            .build()
    }

    /// The `MakerNote` copy of the headroom has to say what the output's ISO
    /// payload says, or `acceptance-criteria.md` §9 is violated by construction.
    #[test]
    fn the_maker_notes_headroom_is_realigned_to_the_output() {
        // Apple's own numbers from the reference capture: 2.2871 stops, where this output
        // derives 2.2681.
        let mn = apple_maker_note((1058474, 1048501), (8699, 165572));
        let src = block_with_apple_maker_note(&mn);
        let mut out = read_bytes(&src).unwrap().unwrap();
        assert!(!out.dropped_maker_note);

        let ours = 2f32.powf(2.268143);
        let before = apple_headroom_in(&out.tiff);
        assert!(
            (before - ours).abs() > HEADROOM_COPIES_TOLERANCE,
            "the source's copy should start out disagreeing, else this proves nothing"
        );
        assert_eq!(
            align_apple_headroom(&mut out.tiff, ours),
            AppleHeadroom::Rewritten
        );
        let after = apple_headroom_in(&out.tiff);
        assert!(
            (after - ours).abs() < HEADROOM_COPIES_TOLERANCE,
            "carried MakerApple headroom {after} still disagrees with {ours}"
        );
    }

    /// Above three stops Apple's formula cannot express the headroom without
    /// understating it, so the tags go rather than lie — and the rest of the
    /// note survives intact.
    #[test]
    fn an_inexpressible_headroom_removes_the_pair_and_keeps_the_rest() {
        let mn = apple_maker_note((1, 1), (8699, 165572));
        let src = block_with_apple_maker_note(&mn);
        let mut out = read_bytes(&src).unwrap().unwrap();

        // 3.568 stops: the Apple-flavor export's headroom, the case §8 is written about.
        let too_much = 2f32.powf(3.568);
        assert_eq!(
            align_apple_headroom(&mut out.tiff, too_much),
            AppleHeadroom::Removed
        );

        let note = carried_maker_note(&out.tiff);
        let entries = apple_maker_entries(&note, false).unwrap();
        let tags: Vec<u16> = entries.iter().map(|(t, ..)| *t).collect();
        assert_eq!(tags, vec![2, 54], "only the headroom pair should be gone");
        // The survivor after the removed pair kept its value, which is the
        // property in-place compaction has to preserve.
        assert_eq!(&note[entries[1].3..entries[1].3 + 4], b"XXXX");
        // And the note is the same length, so the outer entry's count and the
        // pinned offset are both still true.
        assert_eq!(note.len(), mn.len());
    }

    /// A `MakerNote` from any other vendor is not ours to interpret.
    #[test]
    fn a_non_apple_maker_note_is_left_alone() {
        let mut mn = b"Nikon\0\x02\x11\0\0MM\0*".to_vec();
        mn.extend_from_slice(&[0u8; 40]);
        let src = block_with_apple_maker_note(&mn);
        let mut out = read_bytes(&src).unwrap().unwrap();
        let before = out.tiff.clone();
        assert_eq!(
            align_apple_headroom(&mut out.tiff, 4.0),
            AppleHeadroom::Absent
        );
        assert_eq!(out.tiff, before, "nothing should have been touched");
    }

    /// The `MakerNote` value carried in a rebuilt block.
    fn carried_maker_note(block: &[u8]) -> Vec<u8> {
        let tiff = Tiff::open(block).unwrap().unwrap();
        let ifd0 = tiff.read_ifd(tiff.first_ifd).unwrap();
        let exif = tiff
            .read_ifd(tiff.integers(&ifd0.get(TAG_EXIF_IFD).unwrap()).unwrap()[0] as usize)
            .unwrap();
        tiff.bytes_of(&exif.get(TAG_MAKER_NOTE).unwrap())
            .unwrap()
            .to_vec()
    }

    /// What a consumer reading the carried MakerApple tags would compute.
    fn apple_headroom_in(block: &[u8]) -> f32 {
        let note = carried_maker_note(block);
        let entries = apple_maker_entries(&note, false).unwrap();
        let val = |tag: u16| {
            let (.., rel, _) = entries.iter().copied().find(|(t, ..)| *t == tag).unwrap();
            let n = i32::from_be_bytes(note[rel..rel + 4].try_into().unwrap());
            let d = i32::from_be_bytes(note[rel + 4..rel + 8].try_into().unwrap());
            n as f64 / d as f64
        };
        tohdr_core::apple::headroom_from_tags(val(APPLE_TAG_HEADROOM), val(APPLE_TAG_GAIN))
    }

    /// Nothing worth keeping is the same outcome as no Exif at all, not an
    /// error: a conversion must not fail over metadata.
    #[test]
    fn a_source_with_only_pixel_tags_yields_none() {
        let src = Build::new(true).short(259, 1).short(262, 2).build();
        assert!(read_bytes(&src).unwrap().is_none());
    }

    #[test]
    fn unknown_tag_types_are_skipped_not_guessed() {
        let src = Build::new(true).ascii(271, "Apple").bogus_type(306).build();
        let out = read_bytes(&src).unwrap().unwrap();
        let tiff = Tiff::open(&out.tiff).unwrap().unwrap();
        let ifd0 = tiff.read_ifd(tiff.first_ifd).unwrap();
        assert!(ifd0.get(306).is_none());
        assert_eq!(out.tag_count, 1);
    }

    #[test]
    fn non_image_input_is_not_an_error() {
        assert!(read_bytes(b"").unwrap().is_none());
        assert!(read_bytes(b"not an image at all").unwrap().is_none());
        // A JPEG whose only segment is a comment, then the scan.
        assert!(read_bytes(&[0xff, 0xd8, 0xff, 0xfe, 0x00, 0x03, 0x41, 0xff, 0xda])
            .unwrap()
            .is_none());
    }

    /// Odd-length values must not leave the next value on an odd offset.
    #[test]
    fn odd_length_values_stay_aligned() {
        let src = Build::new(true)
            .ascii(271, "Apple") // 6 bytes, even
            .ascii(272, "Nikon Z") // 8 bytes, even
            .ascii(305, "LrC 15") // 7 bytes, odd
            .ascii(306, "2026:06:13 08:29:00") // 20 bytes
            .build();
        let out = read_bytes(&src).unwrap().unwrap();
        let tiff = Tiff::open(&out.tiff).unwrap().unwrap();
        let ifd0 = tiff.read_ifd(tiff.first_ifd).unwrap();
        for (tag, want) in [
            (271u16, &b"Apple\0"[..]),
            (272, &b"Nikon Z\0"[..]),
            (305, &b"LrC 15\0"[..]),
            (306, &b"2026:06:13 08:29:00\0"[..]),
        ] {
            let e = ifd0.get(tag).unwrap();
            assert_eq!(e.value_off % 2, 0, "tag {tag} lands on an odd offset");
            assert_eq!(tiff.bytes_of(&e).unwrap(), want, "tag {tag}");
        }
    }
}
