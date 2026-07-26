//! ISO 21496-1 gain-map metadata serialization (clause C.2.2).
//!
//! Owned by the ISO serializer workstream — see the crate docs. The emitted
//! payload is container-agnostic: HEIC and AVIF both carry these exact bytes as
//! a `tmap` item property, and JXL carries them inside its `jhgm` box.
//!
//! Layout and bit widths are taken from libavif (ground truth for C.2.2):
//! - field order/widths: `avifWriteGainmapMetadata` in `src/write.c:983-1023`
//! - inverse: `avifParseGainMapMetadata` in `src/read.c:2162-2197`
//! - bit packing is MSB-first, and multi-byte ints are big-endian:
//!   `avifRWStreamWriteBits` in `src/stream.c:495-524`
//! - fraction structs (`n`/`d` pairs): `include/avif/avif.h:443-453`
//!
//! All fields below happen to be byte-aligned (the only sub-byte packing is a
//! single flags byte with 2 used bits + 6 reserved), so this is implemented as
//! plain big-endian byte reads/writes rather than a general bitstream.

use crate::GainMapMeta;

/// Fixed-point denominator for our f32<->rational conversion. Chosen over a
/// continued-fraction search (what libavif's `avifDoubleToSignedFraction`
/// does, `src/utils.c:238-294`) for determinism and simplicity: every value
/// quantizes to the nearest multiple of 1/65536 regardless of magnitude.
/// Quantization step is ~1.53e-5, well under f32 ULP for the log2-gain-sized
/// values (roughly -32..32) this format carries; values near the i32 numerator
/// limit (~±32768) lose precision faster as the step is a fixed absolute size,
/// not relative.
const RATIONAL_DENOM: u32 = 1 << 16;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ParseError {
    /// Fewer bytes than the declared layout requires.
    Truncated,
    /// `minimum_version` was nonzero; we only understand version 0's layout.
    UnsupportedVersion(u16),
    /// A fraction's denominator was 0 (undefined value, per `avifGainMapValidateMetadata`, `src/gainmap.c:431-441`).
    ZeroDenominator,
    /// A log2 quantity was far outside anything a real capture produces.
    ///
    /// The numerator is a signed 32-bit value over a fixed 65536 denominator,
    /// so a crafted payload can encode ~32768 stops. `hdr::apply_hdr`
    /// deliberately has no output clamp (it must represent real headroom), so
    /// such a value makes `exp2` overflow and every reconstructed sample
    /// becomes infinite. Rejecting at the parse boundary keeps that from being
    /// the pixel path's problem.
    OutOfRange { field: &'static str, value: f32 },
}

/// Widest log2 range accepted from a file. 64 stops is 1.8e19:1, orders of
/// magnitude beyond any camera, display, or file format in use.
const MAX_ABS_LOG2: f32 = 64.0;

fn check_range(field: &'static str, value: f32) -> Result<f32, ParseError> {
    if !value.is_finite() || value.abs() > MAX_ABS_LOG2 {
        return Err(ParseError::OutOfRange { field, value });
    }
    Ok(value)
}

impl core::fmt::Display for ParseError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Truncated => write!(f, "truncated ISO 21496-1 gain map metadata"),
            Self::UnsupportedVersion(v) => write!(f, "unsupported minimum_version {v}"),
            Self::ZeroDenominator => write!(f, "zero denominator in gain map fraction"),
            Self::OutOfRange { field, value } => {
                write!(f, "{field} out of range: {value}")
            }
        }
    }
}

impl std::error::Error for ParseError {}

fn f32_to_signed_fraction(v: f32) -> (i32, u32) {
    let scaled = (v as f64) * RATIONAL_DENOM as f64;
    let n = scaled.round().clamp(i32::MIN as f64, i32::MAX as f64) as i32;
    (n, RATIONAL_DENOM)
}

/// Serialize a field ISO 21496-1 C.2.2 declares unsigned: the two headroom
/// fields and gamma. libavif types these `avifUnsignedFraction`
/// (`include/avif/avif.h:660,692-693`) against the `avifSignedFraction` used
/// for min/max_log2 and the offsets (`:655-657,669-671`).
///
/// The negative clamp here is a floor of last resort, not a policy. It used to
/// be reached: `derive_consistent` set `alt_headroom = max_log2` unconditionally
/// on the belief the field was signed, so a darkening map wrote `alt_headroom`
/// as 0 while `max_log2` stayed negative and criterion 5 failed on round trip
/// by the full magnitude. The producer now floors, so a negative arriving here
/// means a *new* caller believes something false about the wire format — hence
/// the assertion rather than a silent fix-up.
fn f32_to_unsigned_fraction(v: f32) -> (u32, u32) {
    debug_assert!(
        v >= 0.0,
        "ISO 21496-1 C.2.2 types this field unsigned; a negative value ({v}) \
         cannot be written and would be silently read back as 0. Floor it at \
         the producer, as hdr::derive_consistent does."
    );
    let scaled = (v.max(0.0) as f64) * RATIONAL_DENOM as f64;
    let n = scaled.round().clamp(0.0, u32::MAX as f64) as u32;
    (n, RATIONAL_DENOM)
}

fn fraction_to_f32(n: i64, d: u32) -> Result<f32, ParseError> {
    if d == 0 {
        return Err(ParseError::ZeroDenominator);
    }
    Ok(n as f32 / d as f32)
}

/// True if all three channels carry identical parameters, matching libavif's
/// `avifGainMapIdenticalChannels` (`src/write.c:960-974`), which drives whether
/// 1 or 3 channels get serialized.
fn channels_identical(meta: &GainMapMeta) -> bool {
    meta.min_log2[0] == meta.min_log2[1]
        && meta.min_log2[0] == meta.min_log2[2]
        && meta.max_log2[0] == meta.max_log2[1]
        && meta.max_log2[0] == meta.max_log2[2]
        && meta.gamma[0] == meta.gamma[1]
        && meta.gamma[0] == meta.gamma[2]
        && meta.base_offset[0] == meta.base_offset[1]
        && meta.base_offset[0] == meta.base_offset[2]
        && meta.alt_offset[0] == meta.alt_offset[1]
        && meta.alt_offset[0] == meta.alt_offset[2]
}

/// Serializes clause C.2.2 `GainMapMetadata` (not the outer `ToneMapImage`
/// version byte from ISO/IEC 23008-12 6.6.2.4.2, which callers wrap themselves).
pub fn serialize(meta: &GainMapMeta) -> Vec<u8> {
    let multichannel = !channels_identical(meta);
    let channel_count = if multichannel { 3 } else { 1 };

    let mut buf = Vec::new();
    buf.extend_from_slice(&0u16.to_be_bytes()); // minimum_version, write.c:990
    buf.extend_from_slice(&0u16.to_be_bytes()); // writer_version, write.c:992

    // is_multichannel(1) | use_base_colour_space(1) | reserved(6), write.c:995-997
    let flags = ((multichannel as u8) << 7) | ((meta.use_base_color_space as u8) << 6);
    buf.push(flags);

    let (n, d) = f32_to_unsigned_fraction(meta.base_headroom);
    buf.extend_from_slice(&n.to_be_bytes());
    buf.extend_from_slice(&d.to_be_bytes());
    let (n, d) = f32_to_unsigned_fraction(meta.alt_headroom);
    buf.extend_from_slice(&n.to_be_bytes());
    buf.extend_from_slice(&d.to_be_bytes());

    for c in 0..channel_count {
        let (n, d) = f32_to_signed_fraction(meta.min_log2[c]);
        buf.extend_from_slice(&n.to_be_bytes());
        buf.extend_from_slice(&d.to_be_bytes());
        let (n, d) = f32_to_signed_fraction(meta.max_log2[c]);
        buf.extend_from_slice(&n.to_be_bytes());
        buf.extend_from_slice(&d.to_be_bytes());
        let (n, d) = f32_to_unsigned_fraction(meta.gamma[c]);
        buf.extend_from_slice(&n.to_be_bytes());
        buf.extend_from_slice(&d.to_be_bytes());
        let (n, d) = f32_to_signed_fraction(meta.base_offset[c]);
        buf.extend_from_slice(&n.to_be_bytes());
        buf.extend_from_slice(&d.to_be_bytes());
        let (n, d) = f32_to_signed_fraction(meta.alt_offset[c]);
        buf.extend_from_slice(&n.to_be_bytes());
        buf.extend_from_slice(&d.to_be_bytes());
    }

    buf
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn u16(&mut self) -> Result<u16, ParseError> {
        let b = self
            .bytes
            .get(self.pos..self.pos + 2)
            .ok_or(ParseError::Truncated)?;
        self.pos += 2;
        Ok(u16::from_be_bytes(b.try_into().unwrap()))
    }

    fn u8(&mut self) -> Result<u8, ParseError> {
        let b = *self.bytes.get(self.pos).ok_or(ParseError::Truncated)?;
        self.pos += 1;
        Ok(b)
    }

    fn u32(&mut self) -> Result<u32, ParseError> {
        let b = self
            .bytes
            .get(self.pos..self.pos + 4)
            .ok_or(ParseError::Truncated)?;
        self.pos += 4;
        Ok(u32::from_be_bytes(b.try_into().unwrap()))
    }

    /// Signed 32-bit numerator, stored as the raw two's-complement bit pattern
    /// (see `src/write.c:1006`: cast to `uint32_t` before the bitstream write).
    fn i32(&mut self) -> Result<i32, ParseError> {
        Ok(self.u32()? as i32)
    }
}

/// Parses clause C.2.2 `GainMapMetadata`. Inverse of [`serialize`].
pub fn parse(bytes: &[u8]) -> Result<GainMapMeta, ParseError> {
    let mut r = Reader { bytes, pos: 0 };

    let minimum_version = r.u16()?;
    let _writer_version = r.u16()?; // no corresponding GainMapMeta field to preserve
    if minimum_version != 0 {
        return Err(ParseError::UnsupportedVersion(minimum_version));
    }

    let flags = r.u8()?;
    let is_multichannel = flags & 0x80 != 0;
    let use_base_color_space = flags & 0x40 != 0;
    let channel_count = if is_multichannel { 3 } else { 1 };

    let base_headroom = fraction_to_f32(r.u32()? as i64, r.u32()?)?;
    let alt_headroom = fraction_to_f32(r.u32()? as i64, r.u32()?)?;

    let mut min_log2 = [0.0f32; 3];
    let mut max_log2 = [0.0f32; 3];
    let mut gamma = [0.0f32; 3];
    let mut base_offset = [0.0f32; 3];
    let mut alt_offset = [0.0f32; 3];

    for c in 0..channel_count {
        min_log2[c] = fraction_to_f32(r.i32()? as i64, r.u32()?)?;
        max_log2[c] = fraction_to_f32(r.i32()? as i64, r.u32()?)?;
        gamma[c] = fraction_to_f32(r.u32()? as i64, r.u32()?)?;
        base_offset[c] = fraction_to_f32(r.i32()? as i64, r.u32()?)?;
        alt_offset[c] = fraction_to_f32(r.i32()? as i64, r.u32()?)?;
    }

    // Single-channel payloads replicate channel 0 across RGB, read.c:2191-2197.
    if channel_count == 1 {
        for c in 1..3 {
            min_log2[c] = min_log2[0];
            max_log2[c] = max_log2[0];
            gamma[c] = gamma[0];
            base_offset[c] = base_offset[0];
            alt_offset[c] = alt_offset[0];
        }
    }

    check_range("base_hdr_headroom", base_headroom)?;
    check_range("alternate_hdr_headroom", alt_headroom)?;
    for c in 0..3 {
        check_range("gain_map_min", min_log2[c])?;
        check_range("gain_map_max", max_log2[c])?;
        check_range("gamma", gamma[c])?;
        check_range("base_offset", base_offset[c])?;
        check_range("alternate_offset", alt_offset[c])?;
    }

    Ok(GainMapMeta {
        min_log2,
        max_log2,
        gamma,
        base_offset,
        alt_offset,
        base_headroom,
        alt_headroom,
        use_base_color_space,
    })
}
