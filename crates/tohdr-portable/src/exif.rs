//! Lifting a source file's Exif block out, so a conversion keeps the camera,
//! lens, exposure and date instead of dropping them.
//!
//! Every supported source already carries Exif as one contiguous TIFF
//! structure — a HEIF `Exif` item's payload, a JPEG `APP1` segment's payload,
//! or, for a Lightroom TIFF, the file's own `IFD0`. So this module has one job
//! in three dressings: find that structure, then re-emit it as a standalone
//! TIFF block that [`tohdr_heif::MuxRequest::exif`] can carry.
//!
//! ## Why it is rebuilt rather than copied
//!
//! Copying the bytes would be simpler and is wrong in three ways.
//!
//! A TIFF source's `IFD0` describes *pixels* as well as metadata: its
//! `StripOffsets` point into a file the output does not contain, and its
//! `SubIFDs` pointer reaches the Lightroom gain map. Emitting those verbatim
//! produces an Exif block whose offsets are live pointers into nothing.
//!
//! `Orientation` (`0x0112`) is dropped. Neither loader rotates pixels and
//! [`tohdr_heif`] writes `irot(0)`, so the output's pixels are the source's
//! stored pixels and the container declares no rotation. Copying a source's
//! `Orientation` would put a second, contradicting statement in the file, and
//! for a rotated source two conformant viewers would then disagree about which
//! way up it goes. Reading `Orientation` into `irot`/`imir` is the real fix and
//! is a separate piece of work; until then the file stays self-consistent.
//!
//! `MakerNote` (`0x927C`) is dropped. Apple's — and most vendors' — maker notes
//! address their own contents with offsets relative to the *original* TIFF
//! header, so a maker note that has been relocated no longer parses. Keeping it
//! would trade a missing block for a corrupt one. This also leaves the
//! project's "we do not claim MakerApple headroom tags" position intact; see
//! `docs/acceptance-criteria.md` §8.
//!
//! An embedded ICC profile (`0x8773`) is dropped for the same
//! one-authority reason as `Orientation`: the output states its colour in
//! `colr`, and a second statement that disagreed would be unresolvable.

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
    /// A complete TIFF structure: 8-byte header, `IFD0`, then the Exif and GPS
    /// sub-IFDs it points at, then their values. In the *source's* byte order,
    /// which is what lets every value be copied verbatim.
    pub tiff: Vec<u8>,
    pub origin: Origin,
    /// How many metadata tags survived, excluding the two sub-IFD pointers.
    /// The number a user can compare against `exiftool` output.
    pub tag_count: usize,
}

/// `0x8769`, the pointer to Exif's own IFD.
const TAG_EXIF_IFD: u16 = 34665;
/// `0x8825`, the pointer to the GPS IFD.
const TAG_GPS_IFD: u16 = 34853;
/// `0x927C`. Dropped — see the module docs.
const TAG_MAKER_NOTE: u16 = 37500;
/// `0xA005`, a pointer to the Interoperability IFD. Dropped: it is a third
/// sub-IFD carrying two version tags, and nothing reads them.
const TAG_INTEROP_IFD: u16 = 40965;

/// The `IFD0` tags worth carrying, as an allowlist.
///
/// An allowlist rather than a denylist because the failure directions are not
/// symmetric: forgetting to exclude a pixel-structure tag emits a dangling
/// offset, while forgetting to include a descriptive one merely loses it.
const IFD0_KEEP: &[u16] = &[
    256,   // ImageWidth
    257,   // ImageLength
    270,   // ImageDescription
    271,   // Make
    272,   // Model
    282,   // XResolution
    283,   // YResolution
    296,   // ResolutionUnit
    305,   // Software
    306,   // DateTime
    315,   // Artist
    316,   // HostComputer
    33432, // Copyright
];

/// Reads `path`'s Exif block, or `Ok(None)` if it has none.
///
/// `Ok(None)` covers both "this format carries no Exif" and "this file's Exif
/// is empty", neither of which is a reason to fail a conversion.
pub fn read(path: &Path) -> Result<Option<SourceExif>> {
    let bytes = std::fs::read(path)?;
    read_bytes(&bytes)
}

/// [`read`] on an in-memory file, so tests need no filesystem.
pub fn read_bytes(bytes: &[u8]) -> Result<Option<SourceExif>> {
    let Some((block, origin)) = locate(bytes)? else {
        return Ok(None);
    };
    // A source can carry an Exif item that holds nothing we keep, which is not
    // an error — it is the same outcome as carrying none.
    match rebuild(block)? {
        Some((tiff, tag_count)) => Ok(Some(SourceExif {
            tiff,
            origin,
            tag_count,
        })),
        None => Ok(None),
    }
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

/// One entry's value: either bytes copied from the source, or a sub-IFD offset
/// that is only known once the layout is decided.
enum Value<'a> {
    Bytes(&'a [u8]),
    /// Index into the serialized IFD list, in which 0 is always `IFD0`.
    IfdPointer(usize),
}

struct PlanEntry<'a> {
    tag: u16,
    typ: u16,
    count: u32,
    value: Value<'a>,
}

/// Re-emit `block` as a standalone Exif TIFF, keeping the tags named in the
/// module docs and nothing else.
///
/// `Ok(None)` when nothing survived, which is the same outcome for the caller as
/// a source with no Exif at all.
fn rebuild(block: &[u8]) -> Result<Option<(Vec<u8>, usize)>> {
    let Some(tiff) = Tiff::open(block)? else {
        return Ok(None);
    };
    let ifd0 = tiff.read_ifd(tiff.first_ifd)?;

    // Sub-IFDs are planned first so `IFD0`'s pointer entries can name them by
    // index. Order here is the order they are laid out in the output.
    let mut sub: Vec<Vec<PlanEntry>> = Vec::new();
    let mut exif_idx = None;
    let mut gps_idx = None;
    for (tag, slot) in [(TAG_EXIF_IFD, &mut exif_idx), (TAG_GPS_IFD, &mut gps_idx)] {
        let Some(entry) = ifd0.get(tag) else { continue };
        let Ok(offsets) = tiff.integers(&entry) else {
            continue;
        };
        let Some(&off) = offsets.first() else { continue };
        // A pointer into nothing is a damaged source, not a reason to abandon
        // the metadata that did parse.
        let Ok(sub_ifd) = tiff.read_ifd(off as usize) else {
            continue;
        };
        let kept = plan(&tiff, &sub_ifd, |t| {
            !matches!(t, TAG_MAKER_NOTE | TAG_INTEROP_IFD)
        })?;
        if kept.is_empty() {
            continue;
        }
        // `ifds` below is `[IFD0, ...sub]`, so sub-IFD n is written at index n+1.
        *slot = Some(sub.len() + 1);
        sub.push(kept);
    }

    let mut root = plan(&tiff, &ifd0, |t| IFD0_KEEP.contains(&t))?;
    let tag_count = root.len() + sub.iter().map(Vec::len).sum::<usize>();
    if tag_count == 0 {
        return Ok(None);
    }
    // LONG, count 1 — four bytes, so these live inside the entry and need no
    // space in the value area.
    for (tag, idx) in [(TAG_EXIF_IFD, exif_idx), (TAG_GPS_IFD, gps_idx)] {
        if let Some(i) = idx {
            root.push(PlanEntry {
                tag,
                typ: 4,
                count: 1,
                value: Value::IfdPointer(i),
            });
        }
    }

    let mut ifds = vec![root];
    ifds.extend(sub);
    Ok(Some((serialize(&ifds, tiff.little_endian), tag_count)))
}

/// Copy the entries of one IFD that `keep` accepts.
fn plan<'a>(
    tiff: &Tiff<'a>,
    ifd: &Ifd,
    keep: impl Fn(u16) -> bool,
) -> Result<Vec<PlanEntry<'a>>> {
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

/// Emit the header, the IFDs in order, and one shared value area.
fn serialize(ifds: &[Vec<PlanEntry>], little_endian: bool) -> Vec<u8> {
    let ifd_bytes = |n: usize| 2 + 12 * n + 4;

    // Header is 8 bytes, then every IFD back to back, then values.
    let mut ifd_offsets = Vec::with_capacity(ifds.len());
    let mut at = 8usize;
    for ifd in ifds {
        ifd_offsets.push(at);
        at += ifd_bytes(ifd.len());
    }

    // Values longer than four bytes need somewhere to live. TIFF requires those
    // offsets to be even, and every real value is copied whole, so the only
    // padding is the alignment byte between odd-length neighbours.
    let mut value_offsets: Vec<Vec<usize>> = Vec::with_capacity(ifds.len());
    for ifd in ifds {
        let mut row = Vec::with_capacity(ifd.len());
        for e in ifd {
            match &e.value {
                Value::Bytes(b) if b.len() > 4 => {
                    row.push(at);
                    at += b.len() + b.len() % 2;
                }
                _ => row.push(0),
            }
        }
        value_offsets.push(row);
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
        // TIFF requires entries in ascending tag order; the pointer entries were
        // appended after planning, so this is where that is restored.
        let mut order: Vec<usize> = (0..ifd.len()).collect();
        order.sort_by_key(|&k| ifd[k].tag);

        w.u16(ifd.len() as u16);
        for &k in &order {
            let e = &ifd[k];
            w.u16(e.tag);
            w.u16(e.typ);
            w.u32(e.count);
            match &e.value {
                Value::IfdPointer(target) => w.u32(ifd_offsets[*target] as u32),
                Value::Bytes(b) if b.len() > 4 => w.u32(value_offsets[i][k] as u32),
                Value::Bytes(b) => {
                    // Four bytes or fewer are stored in the entry itself,
                    // left-aligned and zero-filled to the full four.
                    w.buf.extend_from_slice(b);
                    w.buf.extend(std::iter::repeat_n(0u8, 4 - b.len()));
                }
            }
        }
        // No IFD chaining: `IFD0` is the only image and the sub-IFDs are reached
        // by pointer, so every chain terminates here.
        w.u32(0);
    }

    for (i, ifd) in ifds.iter().enumerate() {
        for (k, e) in ifd.iter().enumerate() {
            if let Value::Bytes(b) = &e.value {
                if b.len() > 4 {
                    debug_assert_eq!(w.buf.len(), value_offsets[i][k]);
                    w.buf.extend_from_slice(b);
                    if b.len() % 2 == 1 {
                        w.buf.push(0);
                    }
                }
            }
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
    }

    impl Build {
        fn new(le: bool) -> Self {
            Build {
                le,
                entries: Vec::new(),
                subs: Vec::new(),
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

        /// A tag whose type this reader has no size for.
        fn bogus_type(mut self, tag: u16) -> Self {
            self.entries.push((tag, 999, 1, vec![0, 0, 0, 0]));
            self
        }

        fn sub(mut self, ptr_tag: u16, entries: Vec<(u16, u16, u32, Vec<u8>)>) -> Self {
            self.subs.push((ptr_tag, entries));
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
            root.extend_from_slice(&u32b(0));

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

            let mut out = Vec::new();
            out.extend_from_slice(if self.le { b"II" } else { b"MM" });
            out.extend_from_slice(&u16b(42));
            out.extend_from_slice(&u32b(8));
            out.extend_from_slice(&root);
            out.extend_from_slice(&subs_buf);
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
            (TAG_MAKER_NOTE, 7, 6, b"apple\0".to_vec()),
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
        // Two survivors of three: MakerNote is dropped, so the pointer tag is
        // not counted and neither is it.
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

    #[test]
    fn drops_orientation_maker_note_and_pixel_tags() {
        let src = Build::new(true)
            .ascii(271, "Apple")
            .short(274, 6) // Orientation
            .short(259, 1) // Compression
            .short(262, 2) // PhotometricInterpretation
            .short(330, 9) // SubIFDs — the Lightroom gain map
            .sub(TAG_EXIF_IFD, exif_ifd())
            .build();
        let out = read_bytes(&src).unwrap().unwrap();

        let tiff = Tiff::open(&out.tiff).unwrap().unwrap();
        let ifd0 = tiff.read_ifd(tiff.first_ifd).unwrap();
        for tag in [274u16, 259, 262, 330] {
            assert!(ifd0.get(tag).is_none(), "tag {tag} should be dropped");
        }
        let sub_off = tiff.integers(&ifd0.get(TAG_EXIF_IFD).unwrap()).unwrap()[0];
        let sub = tiff.read_ifd(sub_off as usize).unwrap();
        assert!(sub.get(TAG_MAKER_NOTE).is_none());
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
