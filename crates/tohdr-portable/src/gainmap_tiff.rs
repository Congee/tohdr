//! Reading the gain map out of a Lightroom Classic "HDR Output" TIFF.
//!
//! LrC writes a *pair* in one TIFF, not a single HDR image:
//!
//! - `IFD0` — the SDR rendition, 16-bit, with the export's ICC profile. [`read`]
//!   reads that profile to decide what the output declares.
//! - a SubIFD via tag 330, `PhotometricInterpretation = 52553`,
//!   `NewSubfileType = 32` — a full-res 3-channel 16-bit gain map, plus tag
//!   52557 holding ISO 21496-1 C.2.2 metadata behind a 4-byte zero prefix.
//!
//! Those tag numbers are in no Adobe documentation and exiftool 13.59 does not
//! name 52557; they were read off a real export. See lightroom/README.md.
//!
//! Nothing is tone-mapped here: the headroom is Adobe's and the base is
//! Lightroom's own rendition, so this just hands [`crate::convert`] a
//! `(hdr, base)` pair for [`tohdr_core::hdr::derive_consistent`].
//!
//! The HDR is reconstructed per channel rather than the map being copied,
//! because [`GainPlane`] is single-channel and the three channels disagree by up
//! to 3.43 stops -- picking one visibly shifts saturated highlights. The collapse
//! to mono then happens once, in the code that owns `max_log2 == alt_headroom`.
//!
//! Two inputs are rejected rather than guessed at: the 32-bit float variant
//! (same map, roles reversed -- reconstructing the SDR base would mean shipping
//! *our* application of Adobe's map, and it carries nothing the 16-bit lacks at
//! 1.5x the bytes), and any compression, which the plugin never emits.

use std::path::Path;

use tohdr_core::derive::srgb_to_linear;
use tohdr_core::{colour, iso21496, par, GainMapMeta, HdrRgb, Primaries, Rgb};

use crate::{Error, Result};

/// `PhotometricInterpretation` on the gain-map SubIFD. Undocumented by Adobe
/// and unknown to exiftool 13.59; read off a real LrC 15.4.1 export.
const PHOTOMETRIC_GAIN_MAP: u32 = 52553;

/// TIFF tag carrying ISO 21496-1 C.2.2 metadata on the gain-map SubIFD.
const TAG_GAIN_MAP_METADATA: u16 = 52557;

/// Bytes of `TAG_GAIN_MAP_METADATA` before the C.2.2 payload begins. Observed
/// as four zero bytes in both the 16- and 32-bit exports; the payload is
/// `145 - 4 = 141` bytes, which is exactly C.2.2 for a 3-channel map
/// (`4 + 5 + 8 + 8 + 3 * 40`), with nothing spare.
const METADATA_PREFIX_LEN: usize = 4;

/// `InterColorProfile` — the embedded ICC profile, which is the file's only
/// statement of what its pixels mean. LrC always writes one: a 3144-byte
/// 1998 HP-authored `sRGB IEC61966-2.1` for the HDR sRGB export.
const TAG_ICC_PROFILE: u16 = 34675;

const TAG_IMAGE_WIDTH: u16 = 256;
const TAG_IMAGE_LENGTH: u16 = 257;
const TAG_BITS_PER_SAMPLE: u16 = 258;
const TAG_COMPRESSION: u16 = 259;
const TAG_PHOTOMETRIC: u16 = 262;
const TAG_STRIP_OFFSETS: u16 = 273;
const TAG_SAMPLES_PER_PIXEL: u16 = 277;
const TAG_ROWS_PER_STRIP: u16 = 278;
const TAG_STRIP_BYTE_COUNTS: u16 = 279;
const TAG_PLANAR_CONFIG: u16 = 284;
const TAG_SUB_IFDS: u16 = 330;
const TAG_SAMPLE_FORMAT: u16 = 339;

/// A Lightroom HDR TIFF, decoded into the two images the encoder wants.
pub struct GainMapTiff {
    /// Extended-range scene-linear RGB, `1.0` at diffuse white — the same
    /// convention every other loader in this crate produces. Reconstructed
    /// per channel from the base and the 3-channel map.
    pub hdr: HdrRgb,
    /// Lightroom's own SDR rendition, narrowed to 8 bits in the sRGB-encoded
    /// domain. Not tone-mapped by us.
    pub base: Rgb,
    /// What the file declares, verbatim, before we derive anything. Carried so
    /// callers can report Adobe's headroom next to the one we derive; the two
    /// are different quantities and need not agree.
    pub declared: GainMapMeta,
    /// The primaries `base` and `hdr` are in, read from the embedded ICC profile.
    ///
    /// `None` when the TIFF carries no profile, or one this crate has no matrix
    /// for — an Adobe RGB or ProPhoto export, say. `None` is not "assume sRGB":
    /// assuming would produce a file whose pixels and label disagree, which is
    /// undetectable downstream, so the caller has to decide what to do about it.
    pub primaries: Option<Primaries>,
}

/// Shape and metadata only. Derived `Debug` on this would try to format two
/// buffers that are 722 MiB and 361 MiB at 60 MP, which is not what anybody
/// calling `{:?}` or `expect` wants.
impl core::fmt::Debug for GainMapTiff {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("GainMapTiff")
            .field("width", &self.hdr.width)
            .field("height", &self.hdr.height)
            .field("base_bits", &self.base.bits)
            .field("declared", &self.declared)
            .finish()
    }
}

/// Reads `path` if it is a Lightroom gain-map TIFF, else `Ok(None)`.
///
/// `Ok(None)` means "not this format, try another loader" and is not an error:
/// a plain TIFF, a PNG, a raw file all take that path. An `Err` means the file
/// *is* one of these and something about it could not be honored.
pub fn read(path: &Path) -> Result<Option<GainMapTiff>> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());
    if !matches!(ext.as_deref(), Some("tif") | Some("tiff")) {
        return Ok(None);
    }
    let bytes = std::fs::read(path)?;
    read_bytes(&bytes)
}

/// [`read`] on an in-memory image, so tests need no filesystem.
pub fn read_bytes(bytes: &[u8]) -> Result<Option<GainMapTiff>> {
    let Some(tiff) = Tiff::open(bytes)? else {
        return Ok(None);
    };
    let ifd0 = tiff.read_ifd(tiff.first_ifd)?;

    // Absent tag 330 there is no gain map and nothing to say about the file.
    let Some(sub_entry) = ifd0.get(TAG_SUB_IFDS) else {
        return Ok(None);
    };
    let mut gain_ifd = None;
    for off in tiff.integers(&sub_entry)? {
        let candidate = tiff.read_ifd(off as usize)?;
        let photometric = candidate
            .get(TAG_PHOTOMETRIC)
            .and_then(|e| tiff.integers(&e).ok())
            .and_then(|v| v.first().copied());
        if photometric == Some(PHOTOMETRIC_GAIN_MAP)
            && candidate.get(TAG_GAIN_MAP_METADATA).is_some()
        {
            gain_ifd = Some(candidate);
            break;
        }
    }
    let Some(gain_ifd) = gain_ifd else {
        return Ok(None);
    };

    // From here the file has identified itself, so every problem is an error
    // rather than a polite decline.
    let declared = read_metadata(&tiff, &gain_ifd)?;

    if declared.base_headroom != 0.0 {
        return Err(Error::UnsupportedInput(format!(
            "this is the 32-bit float variant of Lightroom's HDR TIFF: it declares \
             base_hdr_headroom = {:.4} stops and alternate_hdr_headroom = {:.4}, i.e. IFD0 \
             carries the HDR side and the gain map runs downward to SDR. Re-export from \
             Lightroom with Bit Depth 16, which carries the identical gain map with the SDR \
             rendition as the base and is 1.5x smaller",
            declared.base_headroom, declared.alt_headroom
        )));
    }

    let base_plane = Plane::parse(&tiff, &ifd0, "base image")?;
    let gain_plane = Plane::parse(&tiff, &gain_ifd, "gain map")?;

    if base_plane.sample_format != 1 || base_plane.bits != 16 {
        return Err(Error::UnsupportedInput(format!(
            "Lightroom HDR TIFF base image is {}-bit sample format {} (expected 16-bit \
             integer); only the Bit Depth 16 export is supported",
            base_plane.bits, base_plane.sample_format
        )));
    }
    if gain_plane.bits != 16 || gain_plane.sample_format != 1 {
        return Err(Error::UnsupportedInput(format!(
            "gain map is {}-bit sample format {}, expected 16-bit integer",
            gain_plane.bits, gain_plane.sample_format
        )));
    }
    if (gain_plane.width, gain_plane.height) != (base_plane.width, base_plane.height) {
        return Err(Error::UnsupportedInput(format!(
            "gain map is {}x{} against a {}x{} base; only a full-resolution map is \
             supported, which is what LrC 15.4.1 writes",
            gain_plane.width, gain_plane.height, base_plane.width, base_plane.height
        )));
    }

    // Read before reconstructing so a profile problem is reported against the
    // file rather than against pixels we already spent time on.
    let primaries = ifd0
        .get(TAG_ICC_PROFILE)
        .and_then(|e| tiff.bytes_of(&e).ok())
        .and_then(colour::primaries_from_icc);

    let mut out = reconstruct(&base_plane, &gain_plane, declared);
    out.primaries = primaries;
    Ok(Some(out))
}

fn read_metadata(tiff: &Tiff, gain_ifd: &Ifd) -> Result<GainMapMeta> {
    let entry = gain_ifd
        .get(TAG_GAIN_MAP_METADATA)
        .expect("caller checked the tag is present");
    let raw = tiff.bytes_of(&entry)?;
    if raw.len() <= METADATA_PREFIX_LEN {
        return Err(Error::Decode(format!(
            "gain-map metadata tag {TAG_GAIN_MAP_METADATA} is {} bytes, too short to hold a \
             {METADATA_PREFIX_LEN}-byte prefix plus ISO 21496-1 metadata",
            raw.len()
        )));
    }
    iso21496::parse(&raw[METADATA_PREFIX_LEN..]).map_err(|e| {
        // Show the head of the tag: if Adobe ever changes the prefix, that is
        // the one fact needed to work out what it changed to.
        let head: Vec<String> = raw.iter().take(12).map(|b| format!("{b:02x}")).collect();
        Error::Decode(format!(
            "gain-map metadata tag {TAG_GAIN_MAP_METADATA} did not parse as ISO 21496-1 after \
             the {METADATA_PREFIX_LEN}-byte prefix: {e}. First bytes: {}",
            head.join(" ")
        ))
    })
}

/// Builds the `(hdr, base)` pair in two row-parallel passes.
///
/// Both transfer functions are table-driven over *every representable 16-bit
/// code*, which for an integer source is not an approximation of the curve but
/// the curve itself — the same argument `tohdr_core::derive`'s LUTs rest on.
/// That matters here: the naive form is three `powf` and one `exp2` per pixel,
/// or 240 million transcendental calls at 60 MP.
fn reconstruct(base: &Plane<'_>, gain: &Plane<'_>, declared: GainMapMeta) -> GainMapTiff {
    let (w, h) = (base.width, base.height);
    let row_len = w as usize * 3;

    // 16-bit sRGB code -> linear.
    let srgb_lut: Vec<f32> = (0..65536)
        .map(|i| srgb_to_linear(i as f32 / 65535.0))
        .collect();

    // 16-bit gain code -> linear multiplier, per channel. Folding the exp2
    // into the table costs nothing and removes it from the inner loop.
    let scale_lut: Vec<Vec<f32>> = (0..3)
        .map(|c| {
            let min = declared.min_log2[c];
            let range = declared.max_log2[c] - min;
            let inv_gamma = 1.0 / declared.gamma[c].max(1e-6);
            (0..65536)
                .map(|i| {
                    let t = i as f32 / 65535.0;
                    let decoded = if range > 0.0 {
                        min + t.powf(inv_gamma) * range
                    } else {
                        min
                    };
                    decoded.exp2()
                })
                .collect()
        })
        .collect();

    let mut hdr = HdrRgb {
        width: w,
        height: h,
        data: vec![0f32; row_len * h as usize],
    };
    par::for_each_row_chunk_mut(&mut hdr.data, row_len, 1, |start_row, out| {
        for (r, orow) in out.chunks_exact_mut(row_len).enumerate() {
            let y = start_row as u32 + r as u32;
            let brow = base.row(y);
            let grow = gain.row(y);
            for x in 0..w as usize {
                for c in 0..3 {
                    let bv = base.sample(brow, x * 3 + c) as usize;
                    let gv = gain.sample(grow, x * 3 + c) as usize;
                    let lin = srgb_lut[bv];
                    let v = (lin + declared.base_offset[c]) * scale_lut[c][gv]
                        - declared.alt_offset[c];
                    orow[x * 3 + c] = v.max(0.0);
                }
            }
        }
    });

    // Narrowing 16-bit to 8 happens in the sRGB-encoded domain, not through
    // linear: the samples *are* sRGB codes, so this is a pure requantization
    // and needs no transfer function. Rounded, not truncated — `>> 8` would
    // bias every sample downward by up to half a code.
    let mut base8 = vec![0u16; row_len * h as usize];
    par::for_each_row_chunk_mut(&mut base8, row_len, 1, |start_row, out| {
        for (r, orow) in out.chunks_exact_mut(row_len).enumerate() {
            let y = start_row as u32 + r as u32;
            let brow = base.row(y);
            for (i, o) in orow.iter_mut().enumerate() {
                let v = base.sample(brow, i) as u32;
                *o = ((v * 255 + 32767) / 65535) as u16;
            }
        }
    });

    GainMapTiff {
        hdr,
        base: Rgb {
            width: w,
            height: h,
            bits: 8,
            data: base8,
        },
        declared,
        // Filled in by `read`, which is where the IFD is still in hand.
        primaries: None,
    }
}

// ---------------------------------------------------------------------------
// A deliberately small TIFF reader.
//
// The `image` crate reads IFD0 and nothing else, and the `tiff` crate's
// `next_image` walks the IFD *chain* — but a SubIFD referenced by tag 330 is
// not on that chain (`next IFD -> 0` in both real exports), so neither can
// reach the gain map. What is needed is little enough to write out: classic
// TIFF, both byte orders, uncompressed strips.
// ---------------------------------------------------------------------------

// `pub(crate)` rather than private: `crate::exif` rebuilds an Exif block out of
// the same IFDs this module reads pixels from, and a second TIFF parser in the
// same crate would be a second thing to keep correct.
pub(crate) struct Tiff<'a> {
    pub(crate) bytes: &'a [u8],
    pub(crate) little_endian: bool,
    pub(crate) first_ifd: usize,
}

#[derive(Clone, Copy)]
pub(crate) struct Entry {
    pub(crate) typ: u16,
    pub(crate) count: u32,
    /// Absolute offset of the value bytes. For values of four bytes or fewer
    /// TIFF stores them in the entry itself, and then this points at the entry
    /// field — so readers need no special case.
    pub(crate) value_off: usize,
}

pub(crate) struct Ifd {
    entries: Vec<(u16, Entry)>,
    /// Offset of the next IFD on this chain, `0` at the end of it. Zero in both
    /// real LrC exports — but a JPEG's `IFD1`, holding the Exif thumbnail, is
    /// reached only this way, so `crate::exif` needs it.
    pub(crate) next: usize,
}

impl Ifd {
    pub(crate) fn get(&self, tag: u16) -> Option<Entry> {
        self.entries.iter().find(|(t, _)| *t == tag).map(|(_, e)| *e)
    }

    /// Every entry, in the order the file lists them.
    pub(crate) fn entries(&self) -> &[(u16, Entry)] {
        &self.entries
    }
}

pub(crate) fn type_size(typ: u16) -> usize {
    match typ {
        1 | 2 | 6 | 7 => 1,
        3 | 8 => 2,
        4 | 9 | 11 => 4,
        5 | 10 | 12 => 8,
        _ => 0,
    }
}

impl<'a> Tiff<'a> {
    /// `Ok(None)` for anything that is not a classic TIFF, including BigTIFF —
    /// LrC writes classic even for the 1.08 GB 32-bit export.
    pub(crate) fn open(bytes: &'a [u8]) -> Result<Option<Self>> {
        if bytes.len() < 8 {
            return Ok(None);
        }
        let little_endian = match &bytes[0..2] {
            b"II" => true,
            b"MM" => false,
            _ => return Ok(None),
        };
        let t = Tiff {
            bytes,
            little_endian,
            first_ifd: 0,
        };
        if t.u16(2)? != 42 {
            return Ok(None);
        }
        let first_ifd = t.u32(4)? as usize;
        Ok(Some(Tiff { first_ifd, ..t }))
    }

    fn u16(&self, at: usize) -> Result<u16> {
        let s = self.slice(at, 2)?;
        let b = [s[0], s[1]];
        Ok(if self.little_endian {
            u16::from_le_bytes(b)
        } else {
            u16::from_be_bytes(b)
        })
    }

    fn u32(&self, at: usize) -> Result<u32> {
        let s = self.slice(at, 4)?;
        let b = [s[0], s[1], s[2], s[3]];
        Ok(if self.little_endian {
            u32::from_le_bytes(b)
        } else {
            u32::from_be_bytes(b)
        })
    }

    fn slice(&self, at: usize, len: usize) -> Result<&'a [u8]> {
        self.bytes.get(at..at + len).ok_or_else(|| {
            Error::Decode(format!(
                "TIFF truncated: wanted {len} bytes at {at}, file is {} bytes",
                self.bytes.len()
            ))
        })
    }

    pub(crate) fn read_ifd(&self, at: usize) -> Result<Ifd> {
        let n = self.u16(at)? as usize;
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let e = at + 2 + i * 12;
            let tag = self.u16(e)?;
            let typ = self.u16(e + 2)?;
            let count = self.u32(e + 4)?;
            let inline = type_size(typ) * count as usize <= 4;
            let value_off = if inline {
                e + 8
            } else {
                self.u32(e + 8)? as usize
            };
            out.push((tag, Entry { typ, count, value_off }));
        }
        // A truncated chain pointer is not a reason to lose the entries that did
        // parse: an IFD with no readable successor is simply the last one.
        let next = self.u32(at + 2 + n * 12).unwrap_or(0) as usize;
        Ok(Ifd { entries: out, next })
    }

    /// SHORT/LONG (and their signed twins) as `u32`, which is every numeric
    /// tag this module reads.
    pub(crate) fn integers(&self, e: &Entry) -> Result<Vec<u32>> {
        let size = type_size(e.typ);
        if !matches!(e.typ, 1 | 3 | 4 | 8 | 9) || size == 0 {
            return Err(Error::Decode(format!(
                "TIFF tag has type {} where an integer was expected",
                e.typ
            )));
        }
        let mut out = Vec::with_capacity(e.count as usize);
        for i in 0..e.count as usize {
            let at = e.value_off + i * size;
            out.push(match size {
                1 => self.slice(at, 1)?[0] as u32,
                2 => self.u16(at)? as u32,
                _ => self.u32(at)?,
            });
        }
        Ok(out)
    }

    pub(crate) fn bytes_of(&self, e: &Entry) -> Result<&'a [u8]> {
        self.slice(e.value_off, e.count as usize * type_size(e.typ).max(1))
    }
}

/// A TIFF's XMP packet, `IFD0` tag `700`.
///
/// Where Lightroom Classic puts keywords, title, caption, rating, IPTC creator
/// and rights — so for the plugin's own export path this is the only place that
/// metadata exists. `None` for anything that is not a readable TIFF, since a
/// caller asking for XMP has already been told the format.
pub(crate) fn ifd0_xmp(bytes: &[u8]) -> Option<Vec<u8>> {
    /// `0x02BC`.
    const TAG_XMP: u16 = 700;
    let tiff = Tiff::open(bytes).ok()??;
    let ifd0 = tiff.read_ifd(tiff.first_ifd).ok()?;
    let e = ifd0.get(TAG_XMP)?;
    let raw = tiff.bytes_of(&e).ok()?;
    // Written as BYTE or UNDEFINED; a trailing NUL is common and harmless to
    // strip, where leaving it in would appear inside the copied packet.
    let end = raw.iter().rposition(|&b| b != 0).map_or(0, |i| i + 1);
    Some(raw[..end].to_vec()).filter(|v: &Vec<u8>| !v.is_empty())
}

/// One uncompressed, chunky, strip-organized image inside the TIFF.
struct Plane<'a> {
    bytes: &'a [u8],
    little_endian: bool,
    width: u32,
    height: u32,
    bits: u32,
    sample_format: u32,
    row_bytes: usize,
    strip_offsets: Vec<u32>,
    rows_per_strip: u32,
}

impl<'a> Plane<'a> {
    fn parse(tiff: &Tiff<'a>, ifd: &Ifd, what: &str) -> Result<Self> {
        let one = |tag: u16, default: Option<u32>| -> Result<u32> {
            match ifd.get(tag) {
                Some(e) => Ok(tiff.integers(&e)?.first().copied().unwrap_or(0)),
                None => default.ok_or_else(|| {
                    Error::Decode(format!("{what}: TIFF tag {tag} is missing"))
                }),
            }
        };

        let width = one(TAG_IMAGE_WIDTH, None)?;
        let height = one(TAG_IMAGE_LENGTH, None)?;
        let samples = one(TAG_SAMPLES_PER_PIXEL, Some(1))?;
        let compression = one(TAG_COMPRESSION, Some(1))?;
        let planar = one(TAG_PLANAR_CONFIG, Some(1))?;
        let sample_format = one(TAG_SAMPLE_FORMAT, Some(1))?;
        let rows_per_strip = one(TAG_ROWS_PER_STRIP, Some(height))?.max(1);

        let bits_entry = ifd
            .get(TAG_BITS_PER_SAMPLE)
            .ok_or_else(|| Error::Decode(format!("{what}: BitsPerSample is missing")))?;
        let bits_all = tiff.integers(&bits_entry)?;
        let bits = bits_all.first().copied().unwrap_or(0);
        if bits_all.iter().any(|b| *b != bits) {
            return Err(Error::UnsupportedInput(format!(
                "{what}: mixed bit depths per sample ({bits_all:?})"
            )));
        }

        if compression != 1 {
            return Err(Error::UnsupportedInput(format!(
                "{what}: TIFF compression {compression}; only uncompressed is supported, so \
                 export with Compression: None"
            )));
        }
        if planar != 1 {
            return Err(Error::UnsupportedInput(format!(
                "{what}: PlanarConfiguration {planar}; only chunky (1) is supported"
            )));
        }
        if samples != 3 {
            return Err(Error::UnsupportedInput(format!(
                "{what}: {samples} samples per pixel; expected 3"
            )));
        }
        if width == 0 || height == 0 {
            return Err(Error::Decode(format!("{what}: {width}x{height} image")));
        }

        let row_bytes = width as usize * 3 * (bits as usize / 8);
        let strip_offsets = tiff.integers(
            &ifd.get(TAG_STRIP_OFFSETS)
                .ok_or_else(|| Error::Decode(format!("{what}: StripOffsets is missing")))?,
        )?;
        let expected_strips = height.div_ceil(rows_per_strip) as usize;
        if strip_offsets.len() != expected_strips {
            return Err(Error::Decode(format!(
                "{what}: {} strip offsets for {height} rows at {rows_per_strip} rows/strip \
                 (expected {expected_strips})",
                strip_offsets.len()
            )));
        }

        // Bounds-check every strip now so `row` cannot panic later, inside a
        // parallel closure where a panic is far less pleasant to diagnose.
        for (i, off) in strip_offsets.iter().enumerate() {
            let rows = if i + 1 == expected_strips {
                height - i as u32 * rows_per_strip
            } else {
                rows_per_strip
            };
            let need = rows as usize * row_bytes;
            if *off as usize + need > tiff.bytes.len() {
                return Err(Error::Decode(format!(
                    "{what}: strip {i} at offset {off} needs {need} bytes, past the {}-byte \
                     end of the file",
                    tiff.bytes.len()
                )));
            }
        }
        if let Some(e) = ifd.get(TAG_STRIP_BYTE_COUNTS) {
            let counts = tiff.integers(&e)?;
            if let Some(bad) = counts
                .iter()
                .enumerate()
                .find(|(i, c)| **c as usize % row_bytes != 0 && *i < expected_strips)
            {
                return Err(Error::Decode(format!(
                    "{what}: strip {} byte count {} is not a multiple of the {row_bytes}-byte \
                     row stride",
                    bad.0, bad.1
                )));
            }
        }

        Ok(Plane {
            bytes: tiff.bytes,
            little_endian: tiff.little_endian,
            width,
            height,
            bits,
            sample_format,
            row_bytes,
            strip_offsets,
            rows_per_strip,
        })
    }

    #[inline]
    fn row(&self, y: u32) -> &'a [u8] {
        let strip = (y / self.rows_per_strip) as usize;
        let within = (y % self.rows_per_strip) as usize;
        let off = self.strip_offsets[strip] as usize + within * self.row_bytes;
        &self.bytes[off..off + self.row_bytes]
    }

    /// Sample `i` of a row as `u16`. Only called on 16-bit planes; the bit
    /// depth is validated before any pixel is touched.
    #[inline]
    fn sample(&self, row: &[u8], i: usize) -> u16 {
        let b = [row[i * 2], row[i * 2 + 1]];
        if self.little_endian {
            u16::from_le_bytes(b)
        } else {
            u16::from_be_bytes(b)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The gain-map SubIFD also carries `NewSubfileType = 32`, the other
    /// undocumented marker. Detection deliberately does not require it — one
    /// signal (photometric 52553 plus tag 52557) is enough, and demanding both
    /// would turn a cosmetic change on Adobe's side into a hard failure — but
    /// the builder writes it so the fixtures stay faithful to real exports.
    const TAG_NEW_SUBFILE_TYPE: u16 = 254;

    /// The metadata Adobe wrote into a real 16-bit export, as the C.2.2 bytes
    /// with the 4-byte prefix already removed.
    fn adobe_payload() -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&0u16.to_be_bytes()); // minimum_version
        v.extend_from_slice(&0u16.to_be_bytes()); // writer_version
        v.push(0xC0); // is_multichannel | use_base_colour_space
        v.extend_from_slice(&0u32.to_be_bytes()); // base_hdr_headroom = 0/1
        v.extend_from_slice(&1u32.to_be_bytes());
        v.extend_from_slice(&92442u32.to_be_bytes()); // alt = 92442/32768
        v.extend_from_slice(&32768u32.to_be_bytes());
        for (min_n, max_n, gamma_n) in [
            (-26724i32, 92165u32, 512666191u32),
            (-24229, 86297, 508598752),
            (-74351, 103866, 860361581),
        ] {
            v.extend_from_slice(&min_n.to_be_bytes());
            v.extend_from_slice(&32768u32.to_be_bytes());
            v.extend_from_slice(&max_n.to_be_bytes());
            v.extend_from_slice(&32768u32.to_be_bytes());
            v.extend_from_slice(&gamma_n.to_be_bytes());
            v.extend_from_slice(&(1u32 << 30).to_be_bytes());
            v.extend_from_slice(&1i32.to_be_bytes()); // base_offset 1/64
            v.extend_from_slice(&64u32.to_be_bytes());
            v.extend_from_slice(&1i32.to_be_bytes()); // alt_offset 1/64
            v.extend_from_slice(&64u32.to_be_bytes());
        }
        v
    }

    /// Builds a little-endian classic TIFF shaped exactly like a Lightroom
    /// "HDR Output" export: 16-bit RGB IFD0, plus a tag-330 SubIFD with
    /// photometric 52553 and tag 52557.
    struct Builder {
        width: u32,
        height: u32,
        base: Vec<u16>,
        gain: Vec<u16>,
        payload: Vec<u8>,
        photometric_sub: u32,
        base_bits: u16,
        compression: u16,
    }

    impl Builder {
        fn new(width: u32, height: u32) -> Self {
            let n = (width * height * 3) as usize;
            Self {
                width,
                height,
                base: vec![0; n],
                gain: vec![0; n],
                payload: adobe_payload(),
                photometric_sub: PHOTOMETRIC_GAIN_MAP,
                base_bits: 16,
                compression: 1,
            }
        }

        fn build(&self) -> Vec<u8> {
            // Layout, all offsets computed up front because TIFF entries have
            // to point forward at data that does not exist yet:
            //   header | IFD0 | SubIFD | bits triples | prefix+payload | base | gain
            let ifd0_entries = 12u16;
            let sub_entries = 12u16;
            let ifd0_at = 8usize;
            let ifd0_len = 2 + ifd0_entries as usize * 12 + 4;
            let sub_at = ifd0_at + ifd0_len;
            let sub_len = 2 + sub_entries as usize * 12 + 4;
            // BitsPerSample is three SHORTs = 6 bytes, over TIFF's 4-byte
            // inline limit, so real files store it out of line. Match that
            // rather than cheating with count 1.
            let bits0_at = sub_at + sub_len;
            let bits_sub_at = bits0_at + 6;
            let prefix_at = bits_sub_at + 6;
            let payload_at = prefix_at + METADATA_PREFIX_LEN;
            let base_at = payload_at + self.payload.len();
            let base_bytes = self.base.len() * (self.base_bits as usize / 8);
            let gain_at = base_at + base_bytes;
            let gain_bytes = self.gain.len() * 2;

            let mut ifd0 = Vec::new();
            push_entry(&mut ifd0, TAG_NEW_SUBFILE_TYPE, 4, 1, 0);
            push_entry(&mut ifd0, TAG_IMAGE_WIDTH, 4, 1, self.width);
            push_entry(&mut ifd0, TAG_IMAGE_LENGTH, 4, 1, self.height);
            push_entry(&mut ifd0, TAG_BITS_PER_SAMPLE, 3, 3, bits0_at as u32);
            push_entry(&mut ifd0, TAG_COMPRESSION, 3, 1, self.compression as u32);
            push_entry(&mut ifd0, TAG_PHOTOMETRIC, 3, 1, 2);
            push_entry(&mut ifd0, TAG_STRIP_OFFSETS, 4, 1, base_at as u32);
            push_entry(&mut ifd0, TAG_SAMPLES_PER_PIXEL, 3, 1, 3);
            push_entry(&mut ifd0, TAG_ROWS_PER_STRIP, 4, 1, self.height);
            push_entry(&mut ifd0, TAG_STRIP_BYTE_COUNTS, 4, 1, base_bytes as u32);
            push_entry(&mut ifd0, TAG_PLANAR_CONFIG, 3, 1, 1);
            push_entry(&mut ifd0, TAG_SUB_IFDS, 4, 1, sub_at as u32);
            assert_eq!(ifd0.len(), ifd0_entries as usize * 12);

            let mut sub = Vec::new();
            push_entry(&mut sub, TAG_NEW_SUBFILE_TYPE, 4, 1, 32);
            push_entry(&mut sub, TAG_IMAGE_WIDTH, 4, 1, self.width);
            push_entry(&mut sub, TAG_IMAGE_LENGTH, 4, 1, self.height);
            push_entry(&mut sub, TAG_BITS_PER_SAMPLE, 3, 3, bits_sub_at as u32);
            push_entry(&mut sub, TAG_COMPRESSION, 3, 1, 1);
            push_entry(&mut sub, TAG_PHOTOMETRIC, 3, 1, self.photometric_sub);
            push_entry(&mut sub, TAG_STRIP_OFFSETS, 4, 1, gain_at as u32);
            push_entry(&mut sub, TAG_SAMPLES_PER_PIXEL, 3, 1, 3);
            push_entry(&mut sub, TAG_ROWS_PER_STRIP, 4, 1, self.height);
            push_entry(&mut sub, TAG_STRIP_BYTE_COUNTS, 4, 1, gain_bytes as u32);
            push_entry(&mut sub, TAG_PLANAR_CONFIG, 3, 1, 1);
            assert_eq!(sub.len(), (sub_entries as usize - 1) * 12);
            // The metadata tag last, so its 145-byte count and out-of-line
            // offset are easy to see next to the payload it points at.
            push_entry(
                &mut sub,
                TAG_GAIN_MAP_METADATA,
                7,
                (METADATA_PREFIX_LEN + self.payload.len()) as u32,
                prefix_at as u32,
            );

            let mut out = Vec::new();
            out.extend_from_slice(b"II");
            out.extend_from_slice(&42u16.to_le_bytes());
            out.extend_from_slice(&(ifd0_at as u32).to_le_bytes());
            out.extend_from_slice(&ifd0_entries.to_le_bytes());
            out.extend_from_slice(&ifd0);
            out.extend_from_slice(&0u32.to_le_bytes()); // no next IFD
            assert_eq!(out.len(), sub_at);
            out.extend_from_slice(&sub_entries.to_le_bytes());
            out.extend_from_slice(&sub);
            out.extend_from_slice(&0u32.to_le_bytes());
            assert_eq!(out.len(), bits0_at);
            for _ in 0..3 {
                out.extend_from_slice(&self.base_bits.to_le_bytes());
            }
            for _ in 0..3 {
                out.extend_from_slice(&16u16.to_le_bytes());
            }
            assert_eq!(out.len(), prefix_at);
            out.extend_from_slice(&[0u8; METADATA_PREFIX_LEN]);
            out.extend_from_slice(&self.payload);
            assert_eq!(out.len(), base_at);
            for v in &self.base {
                if self.base_bits == 16 {
                    out.extend_from_slice(&v.to_le_bytes());
                } else {
                    out.extend_from_slice(&(*v as f32 / 65535.0).to_le_bytes());
                }
            }
            assert_eq!(out.len(), gain_at);
            for v in &self.gain {
                out.extend_from_slice(&v.to_le_bytes());
            }
            out
        }
    }

    fn push_entry(v: &mut Vec<u8>, tag: u16, typ: u16, count: u32, value: u32) {
        v.extend_from_slice(&tag.to_le_bytes());
        v.extend_from_slice(&typ.to_le_bytes());
        v.extend_from_slice(&count.to_le_bytes());
        // Short inline values sit in the low bytes of the 4-byte field, which
        // is exactly what `to_le_bytes` on the u32 gives for LE files.
        v.extend_from_slice(&value.to_le_bytes());
    }

    #[test]
    fn adobe_payload_parses_to_the_values_read_off_the_real_file() {
        let m = iso21496::parse(&adobe_payload()).expect("parses");
        assert_eq!(m.base_headroom, 0.0);
        assert!((m.alt_headroom - 2.821106).abs() < 1e-5, "{}", m.alt_headroom);
        assert!(m.use_base_color_space);
        assert!((m.max_log2[0] - 2.812653).abs() < 1e-5);
        assert!((m.gamma[2] - 0.801274).abs() < 1e-5);
        for c in 0..3 {
            assert!((m.base_offset[c] - 0.015625).abs() < 1e-6);
        }
    }

    #[test]
    fn plain_tiff_without_the_subifd_is_declined_not_rejected() {
        let mut b = Builder::new(4, 2);
        b.photometric_sub = 2; // an ordinary reduced-resolution page
        let bytes = b.build();
        assert!(read_bytes(&bytes).expect("no error").is_none());
    }

    #[test]
    fn non_tiff_bytes_are_declined() {
        assert!(read_bytes(b"\x89PNG\r\n\x1a\n and then some").unwrap().is_none());
        assert!(read_bytes(b"").unwrap().is_none());
    }

    #[test]
    fn detects_the_gain_map_and_keeps_adobes_declaration() {
        let g = read_bytes(&Builder::new(8, 4).build())
            .expect("reads")
            .expect("is a gain-map TIFF");
        assert_eq!((g.base.width, g.base.height), (8, 4));
        assert_eq!(g.base.bits, 8);
        assert_eq!(g.hdr.data.len(), g.hdr.expected_len());
        assert!((g.declared.alt_headroom - 2.821106).abs() < 1e-5);
    }

    /// The reconstruction is the formula in `derive`'s module docs; check it
    /// against that formula evaluated independently, not against itself.
    #[test]
    fn reconstructs_per_channel_with_the_iso_formula() {
        let mut b = Builder::new(3, 1);
        // A mid-gray base and three different gain codes, so a channel mix-up
        // cannot pass.
        b.base = vec![32768, 32768, 32768, 6553, 6553, 6553, 65535, 65535, 65535];
        b.gain = vec![0, 32768, 65535, 65535, 0, 32768, 20000, 40000, 60000];
        let bytes = b.build();
        let g = read_bytes(&bytes).unwrap().unwrap();
        let m = g.declared;

        for px in 0..3usize {
            for c in 0..3usize {
                let base_code = b.base[px * 3 + c];
                let gain_code = b.gain[px * 3 + c];
                let base_lin = srgb_to_linear(base_code as f32 / 65535.0);
                let range = m.max_log2[c] - m.min_log2[c];
                let decoded = m.min_log2[c]
                    + (gain_code as f32 / 65535.0).powf(1.0 / m.gamma[c]) * range;
                let want = ((base_lin + m.base_offset[c]) * decoded.exp2() - m.alt_offset[c])
                    .max(0.0);
                let got = g.hdr.data[px * 3 + c];
                assert!(
                    (got - want).abs() <= want.abs() * 1e-5 + 1e-6,
                    "px{px} ch{c}: got {got}, want {want}"
                );
            }
        }
    }

    #[test]
    fn base_narrows_to_eight_bits_by_rounding() {
        let mut b = Builder::new(4, 1);
        // 0, just under half a code, just over, and full scale.
        b.base = vec![0; 12];
        b.base[0] = 0;
        b.base[1] = 128; // 128*255/65535 = 0.498 -> 0
        b.base[2] = 129; // 0.502 -> 1
        b.base[3] = 65535; // -> 255
        let g = read_bytes(&b.build()).unwrap().unwrap();
        assert_eq!(g.base.data[0], 0);
        assert_eq!(g.base.data[1], 0);
        assert_eq!(g.base.data[2], 1);
        assert_eq!(g.base.data[3], 255);
    }

    #[test]
    fn hdr_exceeds_diffuse_white_where_the_map_says_it_should() {
        let mut b = Builder::new(2, 1);
        // A near-white base with the map at full scale must land well above
        // 1.0; the same base with the map at its unity point must not.
        b.base = vec![60000; 6];
        b.gain = vec![65535, 65535, 65535, 32134, 31935, 32531];
        let g = read_bytes(&b.build()).unwrap().unwrap();
        assert!(g.hdr.data[0] > 4.0, "full-scale gain gave {}", g.hdr.data[0]);
        for c in 0..3 {
            let v = g.hdr.data[3 + c];
            let base_lin = srgb_to_linear(60000.0 / 65535.0);
            assert!(
                (v - base_lin).abs() < 0.02,
                "unity-gain code ch{c} gave {v}, base linear is {base_lin}"
            );
        }
    }

    #[test]
    fn the_float_variant_is_rejected_with_advice() {
        // Swap the headrooms, as the 32-bit export does.
        let mut payload = adobe_payload();
        payload[5..9].copy_from_slice(&92442u32.to_be_bytes());
        payload[9..13].copy_from_slice(&32768u32.to_be_bytes());
        payload[13..17].copy_from_slice(&0u32.to_be_bytes());
        payload[17..21].copy_from_slice(&1u32.to_be_bytes());
        let mut b = Builder::new(4, 2);
        b.payload = payload;
        let err = read_bytes(&b.build()).expect_err("must reject");
        let msg = err.to_string();
        assert!(msg.contains("Bit Depth 16"), "{msg}");
        assert!(msg.contains("32-bit float"), "{msg}");
    }

    #[test]
    fn compressed_strips_are_rejected_with_advice() {
        let mut b = Builder::new(4, 2);
        b.compression = 5; // LZW
        let err = read_bytes(&b.build()).expect_err("must reject");
        assert!(err.to_string().contains("Compression: None"), "{err}");
    }

    #[test]
    fn truncated_pixel_data_is_an_error_not_a_panic() {
        let bytes = Builder::new(8, 8).build();
        let cut = &bytes[..bytes.len() - 64];
        let err = read_bytes(cut).expect_err("must reject");
        assert!(err.to_string().contains("past the"), "{err}");
    }

    #[test]
    fn big_endian_files_read_identically() {
        // Same content, both byte orders, must reconstruct to the same pixels.
        let mut b = Builder::new(4, 2);
        b.base = (0..24).map(|i| (i * 2500) as u16).collect();
        b.gain = (0..24).map(|i| (65535 - i * 2000) as u16).collect();
        let le = read_bytes(&b.build()).unwrap().unwrap();
        let be = read_bytes(&to_big_endian(&b.build())).unwrap().unwrap();
        assert_eq!(le.hdr.data, be.hdr.data);
        assert_eq!(le.base.data, be.base.data);
    }

    /// Byte-swaps a TIFF built by [`Builder`]: header, every IFD entry's
    /// fixed fields and inline value, and the 16-bit pixel data. Only correct
    /// for the specific shapes `Builder` emits, which is all it is used for.
    fn to_big_endian(le: &[u8]) -> Vec<u8> {
        let mut out = le.to_vec();
        out[0..2].copy_from_slice(b"MM");
        out[2..4].copy_from_slice(&42u16.to_be_bytes());
        let first = u32::from_le_bytes([le[4], le[5], le[6], le[7]]);
        out[4..8].copy_from_slice(&first.to_be_bytes());

        let swap_ifd = |at: usize, out: &mut Vec<u8>| -> usize {
            let n = u16::from_le_bytes([le[at], le[at + 1]]);
            out[at..at + 2].copy_from_slice(&n.to_be_bytes());
            for i in 0..n as usize {
                let e = at + 2 + i * 12;
                let tag = u16::from_le_bytes([le[e], le[e + 1]]);
                let typ = u16::from_le_bytes([le[e + 2], le[e + 3]]);
                let count = u32::from_le_bytes([le[e + 4], le[e + 5], le[e + 6], le[e + 7]]);
                let val = u32::from_le_bytes([le[e + 8], le[e + 9], le[e + 10], le[e + 11]]);
                out[e..e + 2].copy_from_slice(&tag.to_be_bytes());
                out[e + 2..e + 4].copy_from_slice(&typ.to_be_bytes());
                out[e + 4..e + 8].copy_from_slice(&count.to_be_bytes());
                // An inline SHORT sits in the *high* half of the field when
                // big-endian, so re-encode by width rather than as a u32.
                let inline = type_size(typ) * count as usize <= 4;
                if inline && typ == 3 {
                    let v = (val & 0xffff) as u16;
                    out[e + 8..e + 10].copy_from_slice(&v.to_be_bytes());
                    out[e + 10..e + 12].copy_from_slice(&0u16.to_be_bytes());
                } else {
                    out[e + 8..e + 12].copy_from_slice(&val.to_be_bytes());
                }
                // Out-of-line SHORT arrays — BitsPerSample's triple — need
                // their elements swapped too. Missing this made the reader see
                // 16 as 4096 and report a plausible-looking bounds error, so
                // it is worth being explicit rather than special-casing the
                // one tag.
                if !inline && typ == 3 {
                    for k in 0..count as usize {
                        let at = val as usize + k * 2;
                        let v = u16::from_le_bytes([le[at], le[at + 1]]);
                        out[at..at + 2].copy_from_slice(&v.to_be_bytes());
                    }
                }
            }
            let next = at + 2 + n as usize * 12;
            let nx = u32::from_le_bytes([le[next], le[next + 1], le[next + 2], le[next + 3]]);
            out[next..next + 4].copy_from_slice(&nx.to_be_bytes());
            next + 4
        };
        let after0 = swap_ifd(8, &mut out);
        swap_ifd(after0, &mut out);

        // Pixel data: everything from the base strip onward is 16-bit samples.
        let tiff = Tiff::open(le).unwrap().unwrap();
        let ifd0 = tiff.read_ifd(tiff.first_ifd).unwrap();
        let base_at = tiff
            .integers(&ifd0.get(TAG_STRIP_OFFSETS).unwrap())
            .unwrap()[0] as usize;
        let mut i = base_at;
        while i + 1 < out.len() {
            out.swap(i, i + 1);
            i += 2;
        }
        out
    }
}
