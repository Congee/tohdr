//! HEIF/ISOBMFF reader: `HeifFile::parse` and its accessors.
//!
//! Parses just enough of `meta` to locate items, their properties, and their
//! byte ranges — not a general decoder. See the module doc on `lib.rs` for
//! scope.

use crate::boxes::{find_box, iter_boxes, Reader};
use crate::{CodedImage, Error, GainMapInfo, HeifFile, Item, ItemId, Result, APPLE_GAINMAP_URN};
use tohdr_core::iso21496;

/// A decoded `ipco` entry. `Other` covers property types we don't interpret
/// (e.g. `iscl`) — they still occupy a slot, since `ipma` associations index
/// into `ipco` positionally and every property counts toward that index.
// Several variants carry payloads we parse but do not yet consume; keeping
// them decoded means a future reader change is a match arm, not a re-parse.
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub(crate) enum Prop {
    Ispe { width: u32, height: u32 },
    Pixi { bits: Vec<u8> },
    HvcC(Vec<u8>),
    Colr(crate::ColourInfo),
    AuxC(String),
    Clli { max_cll: u16, max_pall: u16 },
    Irot(u8),
    Imir(u8),
    Other,
}

/// One `iloc` item entry, offsets already collapsed to `base_offset + extent
/// offset` (still relative to whichever source `construction_method`
/// selects — file-absolute for 0, `idat`-relative for 1).
#[derive(Clone, Debug)]
pub(crate) struct IlocItem {
    pub item_id: ItemId,
    pub construction_method: u8,
    pub extents: Vec<(u64, u64)>,
}

impl<'a> HeifFile<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self> {
        let len = bytes.len();
        let ftyp = find_box(bytes, 0, len, b"ftyp").ok_or(Error::MissingBox("ftyp"))?;
        let brands = parse_ftyp_brands(bytes, &ftyp)?;
        let meta = find_box(bytes, 0, len, b"meta").ok_or(Error::MissingBox("meta"))?;

        // `meta` is itself a FullBox: 4 bytes of version+flags before its
        // children start.
        if meta.body_end - meta.body_start < 4 {
            return Err(Error::Truncated { at: meta.body_start, need: 4 });
        }
        let children_start = meta.body_start + 4;
        let children_end = meta.body_end;

        let mut primary_item = None;
        let mut items: Vec<Item> = Vec::new();
        let mut props: Vec<Prop> = Vec::new();
        let mut ipma: Vec<(ItemId, Vec<(u16, bool)>)> = Vec::new();
        let mut iloc: Vec<IlocItem> = Vec::new();
        let mut idat: Option<(usize, usize)> = None;
        let mut iref_entries: Vec<([u8; 4], ItemId, Vec<ItemId>)> = Vec::new();

        for b in iter_boxes(bytes, children_start, children_end) {
            match &b.box_type {
                b"pitm" => primary_item = Some(parse_pitm(bytes, &b)?),
                b"iinf" => items = parse_iinf(bytes, &b)?,
                b"iref" => iref_entries = parse_iref(bytes, &b)?,
                b"iprp" => {
                    let (p, m) = parse_iprp(bytes, &b)?;
                    props = p;
                    ipma = m;
                }
                b"idat" => idat = Some((b.body_start, b.body_end)),
                b"iloc" => iloc = parse_iloc(bytes, &b)?,
                // hdlr/dinf/grpl: presence not needed by anything we expose.
                _ => {}
            }
        }

        // Fold `iref` (dimg/auxl) into the items that model them. thmb/cdsc
        // are parsed above (so a file that has them doesn't error) but
        // `Item` has no field for them — nothing else in this crate needs
        // to walk a thumbnail or content-description graph.
        for (ty, from, to_ids) in &iref_entries {
            if let Some(item) = items.iter_mut().find(|i| i.id == *from) {
                match ty {
                    b"dimg" => item.derives_from = to_ids.clone(),
                    b"auxl" => item.auxiliary_to = to_ids.clone(),
                    _ => {}
                }
            }
        }

        // Fold `ipma` into the items' width/height (from `ispe`) and
        // aux_urn (from `auxC`) — the two `Item` fields properties can fill.
        for (item_id, assocs) in &ipma {
            if let Some(item) = items.iter_mut().find(|i| i.id == *item_id) {
                for (idx1, _essential) in assocs {
                    let idx = (*idx1 as usize).wrapping_sub(1);
                    match props.get(idx) {
                        Some(Prop::Ispe { width, height }) => {
                            item.width = Some(*width);
                            item.height = Some(*height);
                        }
                        Some(Prop::AuxC(urn)) => item.aux_urn = Some(urn.clone()),
                        _ => {}
                    }
                }
            }
        }

        Ok(HeifFile {
            bytes,
            brands,
            primary_item,
            items,
            props,
            ipma,
            iloc,
            idat,
        })
    }

    pub fn brands(&self) -> Vec<[u8; 4]> {
        self.brands.clone()
    }

    pub fn primary_item(&self) -> Option<ItemId> {
        self.primary_item
    }

    pub fn items(&self) -> &[Item] {
        &self.items
    }

    pub fn item_data(&self, id: ItemId) -> Result<&'a [u8]> {
        let entry = self
            .iloc
            .iter()
            .find(|e| e.item_id == id)
            .ok_or_else(|| Error::Malformed(format!("no iloc entry for item {id}")))?;
        if entry.extents.is_empty() {
            return Err(Error::Malformed(format!("item {id} has no iloc extents")));
        }

        let mut resolved: Vec<(usize, usize)> = Vec::with_capacity(entry.extents.len());
        for (off, len) in &entry.extents {
            let abs_start: usize = match entry.construction_method {
                0 => usize::try_from(*off)
                    .map_err(|_| Error::Malformed("iloc offset overflow".into()))?,
                1 => {
                    let (idat_start, idat_end) = self.idat.ok_or(Error::MissingBox("idat"))?;
                    let rel = usize::try_from(*off)
                        .map_err(|_| Error::Malformed("iloc offset overflow".into()))?;
                    let start = idat_start
                        .checked_add(rel)
                        .ok_or_else(|| Error::Malformed("idat offset overflow".into()))?;
                    if start > idat_end {
                        return Err(Error::Malformed(format!(
                            "item {id} idat offset past idat end"
                        )));
                    }
                    start
                }
                m => return Err(Error::Unsupported(format!("iloc construction_method {m}"))),
            };
            let length = usize::try_from(*len)
                .map_err(|_| Error::Malformed("iloc length overflow".into()))?;
            let abs_end = abs_start
                .checked_add(length)
                .ok_or_else(|| Error::Malformed("iloc offset+length overflow".into()))?;
            if abs_end > self.bytes.len() {
                return Err(Error::Truncated { at: abs_start, need: length });
            }
            resolved.push((abs_start, abs_end));
        }

        // `item_data` borrows from the original buffer rather than
        // allocating, so multiple extents can only be honored if they are
        // already one contiguous run in memory — which they are for every
        // real file this crate targets (Apple's writer always emits a
        // single extent per non-tiled item). A genuinely scattered item is
        // reported rather than silently copied.
        resolved.sort_by_key(|r| r.0);
        let first_start = resolved[0].0;
        let mut cursor = first_start;
        for (s, e) in &resolved {
            if *s != cursor {
                return Err(Error::Unsupported(
                    "non-contiguous iloc extents (would require an owned copy)".into(),
                ));
            }
            cursor = *e;
        }
        Ok(&self.bytes[first_start..cursor])
    }

    pub fn gain_map(&self) -> Option<GainMapInfo> {
        let apple_item = self
            .items
            .iter()
            .find(|i| i.aux_urn.as_deref() == Some(APPLE_GAINMAP_URN));
        let tmap_item = self.items.iter().find(|i| i.item_type == *b"tmap");

        let (image_item, apple_aux) = match (apple_item, tmap_item) {
            (Some(a), _) => (a.id, true),
            (None, Some(t)) if t.derives_from.len() == 2 => (t.derives_from[1], false),
            _ => return None,
        };

        // The payload is [ToneMapImage version byte][C.2.2 struct] (see
        // `docs/heic-gainmap-structure.md` §5); strip the version byte
        // before handing the rest to `iso21496::parse`, which only knows
        // the bare struct. A malformed/unparseable payload degrades to
        // `None` rather than an error, matching `meta`'s `Option` return —
        // this method reports *where* the gain map is, not whether its
        // metadata is well-formed.
        let meta = tmap_item.and_then(|t| {
            let payload = self.item_data(t.id).ok()?;
            let body = payload.get(1..)?;
            iso21496::parse(body).ok()
        });

        Some(GainMapInfo {
            image_item,
            apple_aux,
            tmap_item: tmap_item.map(|t| t.id),
            meta,
        })
    }

    pub fn coded_image(&self, id: ItemId) -> Result<CodedImage> {
        let item = self
            .items
            .iter()
            .find(|i| i.id == id)
            .ok_or_else(|| Error::Malformed(format!("no such item {id}")))?;
        if item.item_type == *b"grid" {
            return Err(Error::Unsupported(
                "grid item: tile reassembly is a re-encode, not a remux".into(),
            ));
        }

        let data = self.item_data(id)?;

        let assocs = self
            .ipma
            .iter()
            .find(|(iid, _)| *iid == id)
            .map(|(_, a)| a.as_slice())
            .unwrap_or(&[]);

        let mut hvcc = None;
        let mut dims = None;
        let mut bits: Option<Vec<u8>> = None;
        for (idx1, _essential) in assocs {
            let idx = (*idx1 as usize).wrapping_sub(1);
            match self.props.get(idx) {
                Some(Prop::HvcC(v)) => hvcc = Some(v.clone()),
                Some(Prop::Ispe { width, height }) => dims = Some((*width, *height)),
                Some(Prop::Pixi { bits: b }) => bits = Some(b.clone()),
                _ => {}
            }
        }

        let hvcc = hvcc.ok_or(Error::MissingBox("hvcC"))?;
        let (width, height) = dims.ok_or(Error::MissingBox("ispe"))?;
        let bits = bits.unwrap_or_else(|| vec![8]);
        // `hvcC`'s own fields describe the bitstream; `pixi` only describes
        // what a writer *said* about it, and the two can disagree. hpvca emits
        // `pixi` with 3 channels for a plane it coded as 4:0:0, so trusting
        // `pixi` here would launder that error into our own output and fail
        // the single-channel gain-map requirement. Prefer the record, fall
        // back to `pixi` only when it is absent or truncated.
        let cfg = hvcc_config(&hvcc);
        let chroma = match cfg.map(|c| c.chroma_format) {
            Some(0) => crate::Chroma::Monochrome,
            Some(1) => crate::Chroma::Yuv420,
            Some(2) => crate::Chroma::Yuv422,
            Some(3) => crate::Chroma::Yuv444,
            _ if bits.len() == 1 => crate::Chroma::Monochrome,
            _ => crate::Chroma::Yuv420,
        };
        let bit_depth = cfg
            .map(|c| c.bit_depth_luma)
            .unwrap_or_else(|| bits.first().copied().unwrap_or(8));

        Ok(CodedImage {
            width,
            height,
            bit_depth,
            chroma,
            hvcc,
            data: data.to_vec(),
        })
    }
}

/// The fixed-position fields of an `HEVCDecoderConfigurationRecord` that
/// describe the coded format (ISO/IEC 14496-15 §8.3.3.1). Everything past
/// byte 22 is the NAL-unit arrays, which we do not need.
#[derive(Clone, Copy, Debug)]
pub(crate) struct HvccConfig {
    /// Kept for diagnostics: hpvca codes 4:0:0 under Main Still Picture (3),
    /// which mandates 4:2:0, where Apple uses RExt (4).
    #[allow(dead_code)]
    pub profile_idc: u8,
    /// 0 = monochrome, 1 = 4:2:0, 2 = 4:2:2, 3 = 4:4:4.
    pub chroma_format: u8,
    pub bit_depth_luma: u8,
}

/// Read the fixed header of an `hvcC` payload. `None` if it is too short to
/// contain the fields, which is a malformed record rather than a rare
/// variation — but callers here have a `pixi` fallback, so it is not fatal.
pub(crate) fn hvcc_config(hvcc: &[u8]) -> Option<HvccConfig> {
    // Bytes 0..=12 are version/profile/constraints/level, 13..=15 are
    // segmentation and parallelism; the three fields we want start at 16.
    if hvcc.len() < 19 {
        return None;
    }
    Some(HvccConfig {
        profile_idc: hvcc[1] & 0x1F,
        chroma_format: hvcc[16] & 0x03,
        bit_depth_luma: (hvcc[17] & 0x07) + 8,
    })
}

fn parse_ftyp_brands(bytes: &[u8], b: &crate::boxes::BoxHeader) -> Result<Vec<[u8; 4]>> {
    let body = &bytes[b.body_start..b.body_end];
    if body.len() < 8 {
        return Err(Error::Truncated { at: b.body_start, need: 8 });
    }
    let mut out = Vec::new();
    let mut pos = 8;
    while pos + 4 <= body.len() {
        out.push(body[pos..pos + 4].try_into().unwrap());
        pos += 4;
    }
    Ok(out)
}

fn parse_pitm(bytes: &[u8], b: &crate::boxes::BoxHeader) -> Result<ItemId> {
    let mut r = Reader::new(&bytes[b.body_start..b.body_end]);
    let version = r.u8()?;
    r.skip(3)?; // flags
    if version == 0 {
        Ok(r.u16()? as u32)
    } else {
        r.u32()
    }
}

fn parse_infe(bytes: &[u8], b: &crate::boxes::BoxHeader) -> Result<Item> {
    let mut r = Reader::new(&bytes[b.body_start..b.body_end]);
    let version = r.u8()?;
    let flags_bytes = r.take(3)?;
    let flags = u32::from_be_bytes([0, flags_bytes[0], flags_bytes[1], flags_bytes[2]]);
    let hidden = flags & 1 != 0;

    let (id, item_type) = match version {
        // Versions 0/1 predate the `item_type` field entirely (it was only
        // ever implicit, e.g. always an image) — real HEIF writers use v2/v3,
        // so we accept these structurally but can't recover a type string.
        0 | 1 => {
            let id = r.u16()? as u32;
            r.skip(2)?; // item_protection_index
            (id, [0u8; 4])
        }
        2 => {
            let id = r.u16()? as u32;
            r.skip(2)?;
            let it = r.fixed4()?;
            (id, it)
        }
        3 => {
            let id = r.u32()?;
            r.skip(2)?;
            let it = r.fixed4()?;
            (id, it)
        }
        v => return Err(Error::Unsupported(format!("infe version {v}"))),
    };

    Ok(Item {
        id,
        item_type,
        hidden,
        width: None,
        height: None,
        aux_urn: None,
        derives_from: Vec::new(),
        auxiliary_to: Vec::new(),
    })
}

fn parse_iinf(bytes: &[u8], b: &crate::boxes::BoxHeader) -> Result<Vec<Item>> {
    // Just enough of the FullBox header to know where the `infe` children
    // start; we don't need `entry_count` since we iterate the box's actual
    // children regardless of what the header claims.
    if b.body_end - b.body_start < 4 {
        return Err(Error::Truncated { at: b.body_start, need: 4 });
    }
    let version = bytes[b.body_start];
    let count_width = if version == 0 { 2 } else { 4 };
    let children_start = b.body_start + 4 + count_width;

    let mut items = Vec::new();
    for child in iter_boxes(bytes, children_start, b.body_end) {
        if child.box_type == *b"infe" {
            items.push(parse_infe(bytes, &child)?);
        }
    }
    Ok(items)
}

fn parse_iref(
    bytes: &[u8],
    b: &crate::boxes::BoxHeader,
) -> Result<Vec<([u8; 4], ItemId, Vec<ItemId>)>> {
    if b.body_end - b.body_start < 4 {
        return Err(Error::Truncated { at: b.body_start, need: 4 });
    }
    let version = bytes[b.body_start];
    let wide = version != 0;
    let children_start = b.body_start + 4;

    let mut out = Vec::new();
    for child in iter_boxes(bytes, children_start, b.body_end) {
        let mut r = Reader::new(&bytes[child.body_start..child.body_end]);
        let from = if wide { r.u32()? } else { r.u16()? as u32 };
        let count = r.u16()?;
        let mut to_ids = Vec::with_capacity(count as usize);
        for _ in 0..count {
            to_ids.push(if wide { r.u32()? } else { r.u16()? as u32 });
        }
        out.push((child.box_type, from, to_ids));
    }
    Ok(out)
}

fn parse_property(bytes: &[u8], c: &crate::boxes::BoxHeader) -> Result<Prop> {
    let body = &bytes[c.body_start..c.body_end];
    match &c.box_type {
        b"hvcC" => Ok(Prop::HvcC(body.to_vec())),
        b"ispe" => {
            let mut r = Reader::new(body);
            r.skip(4)?; // FullBox version+flags
            let width = r.u32()?;
            let height = r.u32()?;
            Ok(Prop::Ispe { width, height })
        }
        b"pixi" => {
            let mut r = Reader::new(body);
            r.skip(4)?;
            let n = r.u8()? as usize;
            let bits = r.take(n)?.to_vec();
            Ok(Prop::Pixi { bits })
        }
        b"auxC" => {
            let mut r = Reader::new(body);
            r.skip(4)?;
            let rest = r.take(r.remaining())?;
            let nul = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
            let urn = String::from_utf8_lossy(&rest[..nul]).into_owned();
            Ok(Prop::AuxC(urn))
        }
        b"colr" => {
            // Not a FullBox: the colour_type 4CC sits directly at body[0..4].
            let mut r = Reader::new(body);
            let ctype = r.fixed4()?;
            if &ctype == b"nclx" {
                let primaries = r.u16()?;
                let transfer = r.u16()?;
                let matrix = r.u16()?;
                let b = r.u8()?;
                Ok(Prop::Colr(crate::ColourInfo::Nclx {
                    primaries,
                    transfer,
                    matrix,
                    full_range: b & 0x80 != 0,
                }))
            } else {
                // `rICC` or `prof`: everything else is the ICC profile itself.
                let icc = r.take(r.remaining())?.to_vec();
                Ok(Prop::Colr(crate::ColourInfo::Icc(icc)))
            }
        }
        b"clli" => {
            let mut r = Reader::new(body);
            let max_cll = r.u16()?;
            let max_pall = r.u16()?;
            Ok(Prop::Clli { max_cll, max_pall })
        }
        b"irot" => Ok(Prop::Irot(body.first().copied().unwrap_or(0) & 0x3)),
        b"imir" => Ok(Prop::Imir(body.first().copied().unwrap_or(0) & 0x1)),
        _ => Ok(Prop::Other),
    }
}

fn parse_iprp(
    bytes: &[u8],
    b: &crate::boxes::BoxHeader,
) -> Result<(Vec<Prop>, Vec<(ItemId, Vec<(u16, bool)>)>)> {
    let ipco = find_box(bytes, b.body_start, b.body_end, b"ipco").ok_or(Error::MissingBox("ipco"))?;
    let mut props = Vec::new();
    for child in iter_boxes(bytes, ipco.body_start, ipco.body_end) {
        props.push(parse_property(bytes, &child)?);
    }

    let ipma = match find_box(bytes, b.body_start, b.body_end, b"ipma") {
        Some(ib) => parse_ipma(bytes, &ib)?,
        None => Vec::new(),
    };

    Ok((props, ipma))
}

fn parse_ipma(
    bytes: &[u8],
    b: &crate::boxes::BoxHeader,
) -> Result<Vec<(ItemId, Vec<(u16, bool)>)>> {
    let mut r = Reader::new(&bytes[b.body_start..b.body_end]);
    let version = r.u8()?;
    let flags_bytes = r.take(3)?;
    let large_index = flags_bytes[2] & 1 != 0;
    let count = r.u32()?;

    let mut out = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let item_id = if version == 0 { r.u16()? as u32 } else { r.u32()? };
        let assoc_count = r.u8()? as usize;
        let mut assocs = Vec::with_capacity(assoc_count);
        for _ in 0..assoc_count {
            if large_index {
                let v = r.u16()?;
                assocs.push((v & 0x7FFF, v & 0x8000 != 0));
            } else {
                let v = r.u8()?;
                assocs.push(((v & 0x7F) as u16, v & 0x80 != 0));
            }
        }
        out.push((item_id, assocs));
    }
    Ok(out)
}

fn parse_iloc(bytes: &[u8], b: &crate::boxes::BoxHeader) -> Result<Vec<IlocItem>> {
    let mut r = Reader::new(&bytes[b.body_start..b.body_end]);
    let version = r.u8()?;
    r.skip(3)?; // flags

    let b1 = r.u8()?;
    let b2 = r.u8()?;
    let offset_size = (b1 >> 4) as usize;
    let length_size = (b1 & 0xF) as usize;
    let base_offset_size = (b2 >> 4) as usize;
    let index_size = if version == 1 || version == 2 { (b2 & 0xF) as usize } else { 0 };

    let item_count = if version == 2 { r.u32()? } else { r.u16()? as u32 };

    let mut out = Vec::with_capacity(item_count as usize);
    for _ in 0..item_count {
        let item_id = if version == 2 { r.u32()? } else { r.u16()? as u32 };
        let construction_method = if version == 1 || version == 2 {
            (r.u16()? & 0xF) as u8
        } else {
            0
        };
        r.skip(2)?; // data_reference_index — we only support "this file" (0)
        let base_offset = r.uint(base_offset_size)?;
        let extent_count = r.u16()?;
        let mut extents = Vec::with_capacity(extent_count as usize);
        for _ in 0..extent_count {
            if (version == 1 || version == 2) && index_size > 0 {
                r.uint(index_size)?; // extent_index: unused (no item_reference construction)
            }
            let ext_offset = r.uint(offset_size)?;
            let ext_length = r.uint(length_size)?;
            extents.push((base_offset + ext_offset, ext_length));
        }
        out.push(IlocItem { item_id, construction_method, extents });
    }
    Ok(out)
}
