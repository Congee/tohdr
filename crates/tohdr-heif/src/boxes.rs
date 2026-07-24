//! Generic ISOBMFF box framing, shared by the reader and the writer.
//!
//! Nothing here knows about HEIF semantics (no `ispe`/`iloc`/etc.) — just the
//! universal box header (`size, type[, largesize]`) and a couple of
//! bounds-checked cursor helpers so `read.rs` never panics on a truncated or
//! adversarial file.

use crate::{Error, Result};

/// One box header, resolved to absolute byte offsets into the file buffer.
/// `body_start..body_end` is the box's payload, header stripped.
#[derive(Clone, Copy, Debug)]
pub(crate) struct BoxHeader {
    pub box_type: [u8; 4],
    pub body_start: usize,
    pub body_end: usize,
}

/// Reads one box header at `start`, clamped to `end` (the parent's body_end).
///
/// Handles the two irregular size encodings ISOBMFF allows: `size == 1`
/// means a 64-bit `largesize` follows the type, and `size == 0` means "this
/// box runs to the end of its parent" (legal only for a box with no
/// siblings after it, but we honor it either way rather than reject it).
pub(crate) fn read_box_header(bytes: &[u8], start: usize, end: usize) -> Option<BoxHeader> {
    if start.checked_add(8)? > end || start + 8 > bytes.len() {
        return None;
    }
    let size32 = u32::from_be_bytes(bytes[start..start + 4].try_into().unwrap());
    let box_type: [u8; 4] = bytes[start + 4..start + 8].try_into().unwrap();
    let (header_len, size) = if size32 == 1 {
        if start + 16 > end || start + 16 > bytes.len() {
            return None;
        }
        let big = u64::from_be_bytes(bytes[start + 8..start + 16].try_into().unwrap());
        (16usize, big as usize)
    } else if size32 == 0 {
        (8usize, end.saturating_sub(start))
    } else {
        (8usize, size32 as usize)
    };
    if size < header_len {
        return None;
    }
    let box_end = start.checked_add(size)?;
    if box_end > end || box_end > bytes.len() {
        return None;
    }
    Some(BoxHeader {
        box_type,
        body_start: start + header_len,
        body_end: box_end,
    })
}

/// Iterates sibling boxes over `[start, end)`. Stops (rather than erroring)
/// on the first malformed header, so a trailing padding/garbage byte after
/// the last real box doesn't break iteration of everything before it.
pub(crate) fn iter_boxes(bytes: &[u8], start: usize, end: usize) -> BoxIter<'_> {
    BoxIter { bytes, pos: start, end }
}

pub(crate) struct BoxIter<'a> {
    bytes: &'a [u8],
    pos: usize,
    end: usize,
}

impl Iterator for BoxIter<'_> {
    type Item = BoxHeader;

    fn next(&mut self) -> Option<BoxHeader> {
        let h = read_box_header(self.bytes, self.pos, self.end)?;
        self.pos = h.body_end;
        Some(h)
    }
}

pub(crate) fn find_box(bytes: &[u8], start: usize, end: usize, ty: &[u8; 4]) -> Option<BoxHeader> {
    iter_boxes(bytes, start, end).find(|h| &h.box_type == ty)
}

/// Bounds-checked big-endian cursor over one box's body. Every parse function
/// in `read.rs` reads through this instead of raw slicing, so a truncated
/// property or table entry is [`Error::Truncated`] rather than a panic.
pub(crate) struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    pub fn remaining(&self) -> usize {
        self.bytes.len() - self.pos
    }

    pub fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self
            .pos
            .checked_add(n)
            .ok_or(Error::Truncated { at: self.pos, need: n })?;
        let s = self
            .bytes
            .get(self.pos..end)
            .ok_or(Error::Truncated { at: self.pos, need: n })?;
        self.pos = end;
        Ok(s)
    }

    pub fn skip(&mut self, n: usize) -> Result<()> {
        self.take(n).map(|_| ())
    }

    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    pub fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into().unwrap()))
    }

    pub fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }

    pub fn fixed4(&mut self) -> Result<[u8; 4]> {
        Ok(self.take(4)?.try_into().unwrap())
    }

    /// Reads a big-endian unsigned integer of `n` bytes (0..=8), the variable
    /// field width `iloc` uses for offsets/lengths/indices.
    pub fn uint(&mut self, n: usize) -> Result<u64> {
        if n == 0 {
            return Ok(0);
        }
        let s = self.take(n)?;
        let mut acc = 0u64;
        for &b in s {
            acc = (acc << 8) | b as u64;
        }
        Ok(acc)
    }
}

/// Starts a box: reserves the 4-byte size field (patched in [`end_box`]) and
/// writes the type. Returns the buffer position of the size field.
pub(crate) fn begin_box(buf: &mut Vec<u8>, box_type: &[u8; 4]) -> usize {
    let pos = buf.len();
    buf.extend_from_slice(&[0, 0, 0, 0]);
    buf.extend_from_slice(box_type);
    pos
}

/// Same as [`begin_box`] but also writes the `FullBox` version+flags header.
pub(crate) fn begin_fullbox(buf: &mut Vec<u8>, box_type: &[u8; 4], version: u8, flags: u32) -> usize {
    let pos = begin_box(buf, box_type);
    buf.push(version);
    buf.extend_from_slice(&flags.to_be_bytes()[1..]); // flags is 24 bits
    pos
}

/// Backfills the size field reserved by [`begin_box`]/[`begin_fullbox`] now
/// that everything the box contains has been written. This is the writer's
/// half of the two-pass approach the muxer needs throughout: sizes are only
/// known after the fact, but every box's *size* is independent of any
/// `iloc` offset value it might carry (offsets are fixed-width fields), so
/// nesting `begin_box`/`end_box` calls alone is enough to get every box size
/// right in one linear pass — only the file-absolute `iloc` offsets need a
/// second, separate patch (done in `write.rs` after the whole file layout is
/// known).
pub(crate) fn end_box(buf: &mut Vec<u8>, pos: usize) {
    let size = (buf.len() - pos) as u32;
    buf[pos..pos + 4].copy_from_slice(&size.to_be_bytes());
}
