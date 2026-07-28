//! Minimal ISOBMFF/HEIF reader: only the boxes the acceptance criteria name.
//!
//! Deliberately a *second* implementation of something `tohdr-heif` already
//! does. A checker built on our own reader cannot see a bug that our reader and
//! our writer share, so nothing here may reach into it — and the manifest, not
//! discipline, is what enforces that.

use std::collections::HashMap;

pub type Result<T> = core::result::Result<T, String>;

// Every read is bounds-checked and returns an error: the input is a file some
// other program wrote, and half the point of this crate is surviving a bad one.

fn at(b: &[u8], p: usize, n: usize) -> Result<&[u8]> {
    b.get(p..p + n).ok_or_else(|| format!("truncated at {p}+{n} of {}", b.len()))
}

fn be(b: &[u8], p: usize, n: usize) -> Result<u64> {
    let mut v = 0u64;
    for &byte in at(b, p, n)? {
        v = (v << 8) | u64::from(byte);
    }
    Ok(v)
}

fn u8_at(b: &[u8], p: usize) -> Result<u8> {
    Ok(be(b, p, 1)? as u8)
}

fn u16_at(b: &[u8], p: usize) -> Result<u16> {
    Ok(be(b, p, 2)? as u16)
}

fn u32_at(b: &[u8], p: usize) -> Result<u32> {
    Ok(be(b, p, 4)? as u32)
}

fn fourcc(b: &[u8], p: usize) -> Result<[u8; 4]> {
    let s = at(b, p, 4)?;
    Ok([s[0], s[1], s[2], s[3]])
}

/// A null-terminated string and the offset just past its terminator.
fn cstr(b: &[u8], p: usize) -> Result<(String, usize)> {
    let rest = b.get(p..).ok_or_else(|| format!("string starts past end at {p}"))?;
    let n = rest.iter().position(|&c| c == 0).ok_or("unterminated string")?;
    Ok((String::from_utf8_lossy(&rest[..n]).into_owned(), p + n + 1))
}

/// `version` and `flags` of a FullBox, plus the offset of its payload.
fn full(b: &[u8]) -> Result<(u8, u32, usize)> {
    let v = u32_at(b, 0)?;
    Ok(((v >> 24) as u8, v & 0x00ff_ffff, 4))
}

/// Walks the box sequence in `body`. Stops at the first malformed header rather
/// than guessing, which is the honest way to fail on a truncated file.
pub fn boxes(body: &[u8]) -> impl Iterator<Item = ([u8; 4], &[u8])> {
    let mut p = 0usize;
    core::iter::from_fn(move || {
        let size = u32_at(body, p).ok()? as u64;
        let typ = fourcc(body, p + 4).ok()?;
        let (hdr, size) = match size {
            1 => (16usize, be(body, p + 8, 8).ok()?),
            0 => (8usize, (body.len() - p) as u64),
            n => (8usize, n),
        };
        let size = usize::try_from(size).ok()?;
        if size < hdr || p + size > body.len() {
            return None;
        }
        let out = (typ, &body[p + hdr..p + size]);
        p += size;
        Some(out)
    })
}

fn find<'a>(body: &'a [u8], typ: &[u8; 4]) -> Option<&'a [u8]> {
    boxes(body).find(|(t, _)| t == typ).map(|(_, b)| b)
}

/// One entry of `iinf`.
#[derive(Clone, Debug)]
pub struct Item {
    pub id: u32,
    /// `hvc1`, `grid`, `tmap`, `mime`, `Exif`, ... Empty for `infe` version < 2,
    /// which predates item types.
    pub typ: String,
    pub hidden: bool,
    pub content_type: String,
}

impl Item {
    /// Whether this item is a coded or derived *image*, as opposed to metadata.
    /// `iden` and `grid` are derived; `tmap` is deliberately not in the set —
    /// criterion 1 is about the base image, and a `tmap` is never it.
    pub fn is_image(&self) -> bool {
        matches!(self.typ.as_str(), "hvc1" | "hev1" | "av01" | "grid" | "iden" | "jpeg" | "j2k1")
    }
}

/// One `iref` group: a reference type, the item making it, and its targets.
#[derive(Clone, Debug)]
pub struct Reference {
    pub typ: [u8; 4],
    pub from: u32,
    pub to: Vec<u32>,
}

/// The properties criterion 2 and 3 read. Everything else is kept only by name,
/// so `ipma` indices stay aligned with `ipco` order.
#[derive(Clone, Debug)]
pub enum Prop {
    Ispe { width: u32, height: u32 },
    Pixi { bits: Vec<u8> },
    AuxC { urn: String },
    Other([u8; 4]),
}

/// One `grpl` entity group, e.g. `altr`.
#[derive(Clone, Debug)]
pub struct Group {
    pub typ: [u8; 4],
    pub id: u32,
    pub entities: Vec<u32>,
}

struct Extent {
    offset: u64,
    length: u64,
}

struct Location {
    method: u8,
    base: u64,
    extents: Vec<Extent>,
}

pub struct Heif<'a> {
    bytes: &'a [u8],
    pub brands: Vec<String>,
    pub primary: Option<u32>,
    pub items: Vec<Item>,
    pub refs: Vec<Reference>,
    pub props: Vec<Prop>,
    /// item id -> the `ipco` indices associated with it (1-based, as stored).
    pub assoc: HashMap<u32, Vec<u16>>,
    pub groups: Vec<Group>,
    idat: Vec<u8>,
    locs: HashMap<u32, Location>,
}

impl<'a> Heif<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self> {
        let mut f = Heif {
            bytes,
            brands: Vec::new(),
            primary: None,
            items: Vec::new(),
            refs: Vec::new(),
            props: Vec::new(),
            assoc: HashMap::new(),
            groups: Vec::new(),
            idat: Vec::new(),
            locs: HashMap::new(),
        };

        let ftyp = find(bytes, b"ftyp").ok_or("no ftyp box")?;
        f.brands.push(String::from_utf8_lossy(at(ftyp, 0, 4)?).into_owned());
        let mut p = 8;
        while let Ok(b) = at(ftyp, p, 4) {
            f.brands.push(String::from_utf8_lossy(b).into_owned());
            p += 4;
        }

        let meta = find(bytes, b"meta").ok_or("no meta box")?;
        // `meta` is a FullBox; its children start after version+flags.
        let (_, _, body) = full(meta)?;
        let meta = &meta[body..];

        for (typ, b) in boxes(meta) {
            match &typ {
                b"pitm" => f.primary = Some(f.parse_pitm(b)?),
                b"iinf" => f.items = parse_iinf(b)?,
                b"iref" => f.refs = parse_iref(b)?,
                b"iprp" => f.parse_iprp(b)?,
                b"grpl" => f.groups = parse_grpl(b)?,
                b"idat" => f.idat = b.to_vec(),
                b"iloc" => f.locs = parse_iloc(b)?,
                _ => {}
            }
        }
        Ok(f)
    }

    fn parse_pitm(&self, b: &[u8]) -> Result<u32> {
        let (version, _, p) = full(b)?;
        if version == 0 { Ok(u32::from(u16_at(b, p)?)) } else { u32_at(b, p) }
    }

    fn parse_iprp(&mut self, b: &[u8]) -> Result<()> {
        if let Some(ipco) = find(b, b"ipco") {
            for (typ, pb) in boxes(ipco) {
                self.props.push(match &typ {
                    b"ispe" => {
                        let (_, _, p) = full(pb)?;
                        Prop::Ispe { width: u32_at(pb, p)?, height: u32_at(pb, p + 4)? }
                    }
                    b"pixi" => {
                        let (_, _, p) = full(pb)?;
                        let n = usize::from(u8_at(pb, p)?);
                        Prop::Pixi { bits: at(pb, p + 1, n)?.to_vec() }
                    }
                    b"auxC" => {
                        let (_, _, p) = full(pb)?;
                        Prop::AuxC { urn: cstr(pb, p)?.0 }
                    }
                    other => Prop::Other(*other),
                });
            }
        }
        if let Some(ipma) = find(b, b"ipma") {
            self.assoc = parse_ipma(ipma)?;
        }
        Ok(())
    }

    pub fn item(&self, id: u32) -> Option<&Item> {
        self.items.iter().find(|i| i.id == id)
    }

    /// Properties associated with `id`, in association order.
    pub fn props_of(&self, id: u32) -> Vec<&Prop> {
        self.assoc
            .get(&id)
            .into_iter()
            .flatten()
            .filter_map(|&i| self.props.get(usize::from(i).checked_sub(1)?))
            .collect()
    }

    pub fn refs_from(&self, from: u32, typ: &[u8; 4]) -> Option<&Reference> {
        self.refs.iter().find(|r| r.from == from && &r.typ == typ)
    }

    /// Item ids that reference `to` with `typ`.
    pub fn refs_to(&self, to: u32, typ: &[u8; 4]) -> Vec<u32> {
        self.refs
            .iter()
            .filter(|r| &r.typ == typ && r.to.contains(&to))
            .map(|r| r.from)
            .collect()
    }

    /// An item's bytes, following `iloc`'s construction method: 0 is a file
    /// offset, 1 is an offset into `idat`. Method 2 (another item) is not used
    /// by any file this checks and is rejected rather than guessed at.
    pub fn item_data(&self, id: u32) -> Result<Vec<u8>> {
        let loc = self.locs.get(&id).ok_or_else(|| format!("item {id} has no iloc entry"))?;
        let src: &[u8] = match loc.method {
            0 => self.bytes,
            1 => &self.idat,
            m => return Err(format!("item {id}: construction method {m} unsupported")),
        };
        let mut out = Vec::new();
        for e in &loc.extents {
            let start = usize::try_from(loc.base + e.offset).map_err(|_| "offset overflow")?;
            let len = usize::try_from(e.length).map_err(|_| "length overflow")?;
            // A zero length means "to the end of the source" in some writers'
            // output; nothing here relies on it, so treat it as exactly zero.
            out.extend_from_slice(at(src, start, len)?);
        }
        Ok(out)
    }
}

fn parse_iinf(b: &[u8]) -> Result<Vec<Item>> {
    let (version, _, p) = full(b)?;
    let (count, p) = if version == 0 {
        (u32::from(u16_at(b, p)?), p + 2)
    } else {
        (u32_at(b, p)?, p + 4)
    };
    let mut out = Vec::new();
    for (typ, ib) in boxes(&b[p..]).take(count as usize) {
        if &typ != b"infe" {
            continue;
        }
        let (version, flags, q) = full(ib)?;
        let (id, q) = if version >= 3 {
            (u32_at(ib, q)?, q + 4)
        } else {
            (u32::from(u16_at(ib, q)?), q + 2)
        };
        let q = q + 2; // item_protection_index
        let mut item = Item {
            id,
            typ: String::new(),
            hidden: version >= 2 && flags & 1 != 0,
            content_type: String::new(),
        };
        if version >= 2 {
            item.typ = String::from_utf8_lossy(at(ib, q, 4)?).into_owned();
            let (_name, q) = cstr(ib, q + 4)?;
            if item.typ == "mime" {
                item.content_type = cstr(ib, q)?.0;
            }
        }
        out.push(item);
    }
    Ok(out)
}

fn parse_iref(b: &[u8]) -> Result<Vec<Reference>> {
    let (version, _, p) = full(b)?;
    let wide = version >= 1;
    let id = |b: &[u8], p: usize| -> Result<u32> {
        if wide { u32_at(b, p) } else { Ok(u32::from(u16_at(b, p)?)) }
    };
    let step = if wide { 4 } else { 2 };
    let mut out = Vec::new();
    for (typ, rb) in boxes(&b[p..]) {
        let from = id(rb, 0)?;
        let count = usize::from(u16_at(rb, step)?);
        let mut to = Vec::with_capacity(count);
        for i in 0..count {
            to.push(id(rb, step + 2 + i * step)?);
        }
        out.push(Reference { typ, from, to });
    }
    Ok(out)
}

fn parse_ipma(b: &[u8]) -> Result<HashMap<u32, Vec<u16>>> {
    let (version, flags, mut p) = full(b)?;
    let count = u32_at(b, p)?;
    p += 4;
    let mut out = HashMap::new();
    for _ in 0..count {
        let id = if version < 1 {
            let v = u32::from(u16_at(b, p)?);
            p += 2;
            v
        } else {
            let v = u32_at(b, p)?;
            p += 4;
            v
        };
        let n = usize::from(u8_at(b, p)?);
        p += 1;
        let mut idx = Vec::with_capacity(n);
        for _ in 0..n {
            // The essential bit is the high bit either way; only the index
            // width changes with flags bit 0.
            if flags & 1 != 0 {
                idx.push(u16_at(b, p)? & 0x7fff);
                p += 2;
            } else {
                idx.push(u16::from(u8_at(b, p)? & 0x7f));
                p += 1;
            }
        }
        out.insert(id, idx);
    }
    Ok(out)
}

fn parse_grpl(b: &[u8]) -> Result<Vec<Group>> {
    let mut out = Vec::new();
    for (typ, gb) in boxes(b) {
        let (_, _, p) = full(gb)?;
        let id = u32_at(gb, p)?;
        let n = u32_at(gb, p + 4)? as usize;
        let mut entities = Vec::with_capacity(n);
        for i in 0..n {
            entities.push(u32_at(gb, p + 8 + i * 4)?);
        }
        out.push(Group { typ, id, entities });
    }
    Ok(out)
}

fn parse_iloc(b: &[u8]) -> Result<HashMap<u32, Location>> {
    let (version, _, mut p) = full(b)?;
    let sizes = u8_at(b, p)?;
    let (offset_size, length_size) = (usize::from(sizes >> 4), usize::from(sizes & 0xf));
    let sizes = u8_at(b, p + 1)?;
    let (base_size, index_size) = (usize::from(sizes >> 4), usize::from(sizes & 0xf));
    p += 2;
    let count = if version < 2 {
        let v = u32::from(u16_at(b, p)?);
        p += 2;
        v
    } else {
        let v = u32_at(b, p)?;
        p += 4;
        v
    };
    let mut out = HashMap::new();
    for _ in 0..count {
        let id = if version < 2 {
            let v = u32::from(u16_at(b, p)?);
            p += 2;
            v
        } else {
            let v = u32_at(b, p)?;
            p += 4;
            v
        };
        let mut method = 0u8;
        if version == 1 || version == 2 {
            method = (u16_at(b, p)? & 0xf) as u8;
            p += 2;
        }
        p += 2; // data_reference_index
        let base = be(b, p, base_size)?;
        p += base_size;
        let n = usize::from(u16_at(b, p)?);
        p += 2;
        let mut extents = Vec::with_capacity(n);
        for _ in 0..n {
            if (version == 1 || version == 2) && index_size > 0 {
                p += index_size;
            }
            let offset = be(b, p, offset_size)?;
            p += offset_size;
            let length = be(b, p, length_size)?;
            p += length_size;
            extents.push(Extent { offset, length });
        }
        out.insert(id, Location { method, base, extents });
    }
    Ok(out)
}
