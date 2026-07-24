//! Gain-map HEIC muxer: [`mux`].
//!
//! # Two-pass offsets
//!
//! `iloc` extents that point into `mdat` (`construction_method` 0) need the
//! *absolute file offset* of each image's bytes, but that offset depends on
//! the size of everything before `mdat` — including `iloc` itself. We break
//! the cycle by exploiting that a box's serialized *size* never depends on
//! the *value* written into a fixed-width field: we write `iloc` with
//! placeholder offsets (0) and record where those 4-byte fields landed in
//! the output buffer, finish serializing `meta` (now its length is known),
//! compute `mdat`'s body start from `ftyp_len + meta_len + 8`, and go back
//! and patch the placeholders with the real absolute offsets. One pass to
//! build, one small patch pass — no separate size-accounting phase needed.
//!
//! `idat`-relative extents (`construction_method` 1) don't have this
//! problem: they're relative to `idat`'s own body, which we control and
//! know immediately as we lay out its contents.

use crate::boxes::{begin_box, begin_fullbox, end_box};
use crate::{Chroma, ColourInfo, MuxRequest, Result};
use std::collections::BTreeMap;

pub(crate) fn mux(req: &MuxRequest) -> Result<Vec<u8>> {
    let base_id: u32 = 1;
    let gain_id: u32 = 2;
    let mut next_id = 3u32;
    let tmap_id = req.flavor.writes_iso().then(|| {
        let id = next_id;
        next_id += 1;
        id
    });
    let exif_id = req.exif.as_ref().map(|_| {
        let id = next_id;
        next_id += 1;
        id
    });
    let xmp_id = req.xmp.as_ref().map(|_| {
        let id = next_id;
        next_id += 1;
        id
    });

    // --- mdat: the two coded HEVC bitstreams, contiguous ---
    let mut mdat_body = Vec::new();
    let base_rel = mdat_body.len() as u64;
    mdat_body.extend_from_slice(&req.base.data);
    let gain_rel = mdat_body.len() as u64;
    mdat_body.extend_from_slice(&req.gain.data);

    // --- idat: small item-owned metadata (tmap payload, Exif, XMP) ---
    let mut idat_body = Vec::new();
    let mut idat_entries: Vec<(u32, u64, u64)> = Vec::new(); // (item_id, offset, length)
    if let Some(id) = tmap_id {
        // ToneMapImage version byte (ISO/IEC 23008-12 6.6.2.4.2), then the
        // bare C.2.2 struct `iso21496::serialize` emits — see that module's
        // doc comment for why the two are split.
        let mut payload = vec![0u8];
        payload.extend_from_slice(&tohdr_core::iso21496::serialize(&req.meta));
        let off = idat_body.len() as u64;
        let len = payload.len() as u64;
        idat_body.extend_from_slice(&payload);
        idat_entries.push((id, off, len));
    }
    if let (Some(id), Some(exif)) = (exif_id, &req.exif) {
        let off = idat_body.len() as u64;
        // 4-byte exif_tiff_header_offset the `Exif` item type requires
        // ahead of the actual TIFF header; we always emit 0 (no prefix).
        idat_body.extend_from_slice(&0u32.to_be_bytes());
        idat_body.extend_from_slice(exif);
        let len = idat_body.len() as u64 - off;
        idat_entries.push((id, off, len));
    }
    if let (Some(id), Some(xmp)) = (xmp_id, &req.xmp) {
        let off = idat_body.len() as u64;
        idat_body.extend_from_slice(xmp);
        let len = idat_body.len() as u64 - off;
        idat_entries.push((id, off, len));
    }

    // --- ftyp ---
    let mut brands = vec![*b"mif1", *b"heic", *b"miaf"];
    if req.flavor.writes_iso() {
        brands.push(*b"tmap");
    }
    let ftyp_bytes = write_ftyp(b"heic", 0, &brands);

    // --- ipco/ipma: properties, and which items they attach to ---
    let mut props: Vec<Vec<u8>> = Vec::new();
    let mut assocs: Vec<(u32, u16, bool)> = Vec::new();

    props.push(ispe_box(req.base.width, req.base.height));
    assocs.push((base_id, props.len() as u16, false));
    props.push(pixi_box(pixi_channels(req.base.chroma), req.base.bit_depth));
    assocs.push((base_id, props.len() as u16, false));
    props.push(hvcc_box(&req.base.hvcc));
    assocs.push((base_id, props.len() as u16, true));
    if let Some(c) = &req.base_colour {
        props.push(colr_box(c));
        assocs.push((base_id, props.len() as u16, true));
    }

    props.push(ispe_box(req.gain.width, req.gain.height));
    assocs.push((gain_id, props.len() as u16, false));
    props.push(pixi_box(pixi_channels(req.gain.chroma), req.gain.bit_depth));
    assocs.push((gain_id, props.len() as u16, false));
    props.push(hvcc_box(&req.gain.hvcc));
    assocs.push((gain_id, props.len() as u16, true));
    if req.flavor.writes_apple() {
        props.push(auxc_box(crate::APPLE_GAINMAP_URN));
        assocs.push((gain_id, props.len() as u16, true));
    }

    if let Some(id) = tmap_id {
        // Every image item, derived ones included, is expected to carry an
        // `ispe` per MIAF/HEIF; non-essential since it just restates the
        // base's own dimensions for generic-item-enumeration convenience.
        props.push(ispe_box(req.base.width, req.base.height));
        assocs.push((id, props.len() as u16, false));
        if let Some(c) = &req.tmap_colour {
            props.push(colr_box(c));
            assocs.push((id, props.len() as u16, true));
        }
    }

    if let Some((max_cll, max_pall)) = req.clli {
        // CLLI describes the reconstructed content as a whole; attached to
        // the base item since that's the item every decoder will resolve
        // regardless of gain-map support.
        props.push(clli_box(max_cll, max_pall));
        assocs.push((base_id, props.len() as u16, false));
    }

    // --- iref ---
    let mut iref_entries: Vec<([u8; 4], u32, Vec<u32>)> = Vec::new();
    if req.flavor.writes_apple() {
        iref_entries.push((*b"auxl", gain_id, vec![base_id]));
    }
    if let Some(id) = tmap_id {
        // Base then gain map, in that order — see docs §2: this is the
        // order a decoder walks to do ISO-standard reconstruction.
        iref_entries.push((*b"dimg", id, vec![base_id, gain_id]));
    }
    let mut cdsc_targets = vec![base_id];
    if let Some(id) = tmap_id {
        cdsc_targets.push(id);
    }
    if let Some(id) = exif_id {
        iref_entries.push((*b"cdsc", id, cdsc_targets.clone()));
    }
    if let Some(id) = xmp_id {
        iref_entries.push((*b"cdsc", id, cdsc_targets.clone()));
    }

    // --- assemble meta ---
    let mut meta = Vec::new();
    let meta_pos = begin_fullbox(&mut meta, b"meta", 0, 0);
    write_hdlr(&mut meta);
    write_pitm(&mut meta, base_id);
    write_iinf(&mut meta, base_id, gain_id, tmap_id, exif_id, xmp_id);
    if !iref_entries.is_empty() {
        write_iref(&mut meta, &iref_entries);
    }
    write_iprp(&mut meta, &props, &assocs);
    if !idat_body.is_empty() {
        let p = begin_box(&mut meta, b"idat");
        meta.extend_from_slice(&idat_body);
        end_box(&mut meta, p);
    }
    let file_entries = [
        (base_id, base_rel, req.base.data.len() as u64),
        (gain_id, gain_rel, req.gain.data.len() as u64),
    ];
    let patch_positions = write_iloc(&mut meta, &file_entries, &idat_entries);
    end_box(&mut meta, meta_pos);

    // --- patch construction_method-0 offsets now that mdat's start is known ---
    let mdat_header_len = 8usize;
    let mdat_body_start = ftyp_bytes.len() + meta.len() + mdat_header_len;
    for (pos, rel) in &patch_positions {
        let abs = mdat_body_start as u64 + rel;
        meta[*pos..*pos + 4].copy_from_slice(&(abs as u32).to_be_bytes());
    }

    let mut out = Vec::with_capacity(ftyp_bytes.len() + meta.len() + mdat_header_len + mdat_body.len());
    out.extend_from_slice(&ftyp_bytes);
    out.extend_from_slice(&meta);
    let mdat_pos = begin_box(&mut out, b"mdat");
    out.extend_from_slice(&mdat_body);
    end_box(&mut out, mdat_pos);

    Ok(out)
}

fn pixi_channels(chroma: Chroma) -> u8 {
    if chroma == Chroma::Monochrome {
        1
    } else {
        3
    }
}

fn write_ftyp(major: &[u8; 4], minor: u32, compat: &[[u8; 4]]) -> Vec<u8> {
    let mut buf = Vec::new();
    let p = begin_box(&mut buf, b"ftyp");
    buf.extend_from_slice(major);
    buf.extend_from_slice(&minor.to_be_bytes());
    for c in compat {
        buf.extend_from_slice(c);
    }
    end_box(&mut buf, p);
    buf
}

fn write_hdlr(buf: &mut Vec<u8>) {
    let p = begin_fullbox(buf, b"hdlr", 0, 0);
    buf.extend_from_slice(&0u32.to_be_bytes()); // pre_defined
    buf.extend_from_slice(b"pict"); // handler_type: image (HEIF/MIAF requirement)
    buf.extend_from_slice(&[0u8; 12]); // reserved
    buf.push(0); // empty name
    end_box(buf, p);
}

fn write_pitm(buf: &mut Vec<u8>, id: u32) {
    let p = begin_fullbox(buf, b"pitm", 0, 0);
    buf.extend_from_slice(&(id as u16).to_be_bytes());
    end_box(buf, p);
}

fn write_infe(buf: &mut Vec<u8>, id: u32, item_type: [u8; 4], hidden: bool, content_type: Option<&str>) {
    let flags = if hidden { 1u32 } else { 0 };
    let p = begin_fullbox(buf, b"infe", 2, flags);
    buf.extend_from_slice(&(id as u16).to_be_bytes());
    buf.extend_from_slice(&0u16.to_be_bytes()); // item_protection_index
    buf.extend_from_slice(&item_type);
    buf.push(0); // empty item_name
    if item_type == *b"mime" {
        let ct = content_type.unwrap_or("application/octet-stream");
        buf.extend_from_slice(ct.as_bytes());
        buf.push(0);
    }
    end_box(buf, p);
}

fn write_iinf(
    buf: &mut Vec<u8>,
    base_id: u32,
    gain_id: u32,
    tmap_id: Option<u32>,
    exif_id: Option<u32>,
    xmp_id: Option<u32>,
) {
    let mut count = 2u16;
    count += tmap_id.is_some() as u16;
    count += exif_id.is_some() as u16;
    count += xmp_id.is_some() as u16;

    let p = begin_fullbox(buf, b"iinf", 0, 0);
    buf.extend_from_slice(&count.to_be_bytes());
    write_infe(buf, base_id, *b"hvc1", false, None);
    write_infe(buf, gain_id, *b"hvc1", true, None);
    if let Some(id) = tmap_id {
        write_infe(buf, id, *b"tmap", false, None);
    }
    if let Some(id) = exif_id {
        write_infe(buf, id, *b"Exif", true, None);
    }
    if let Some(id) = xmp_id {
        write_infe(buf, id, *b"mime", true, Some("application/rdf+xml"));
    }
    end_box(buf, p);
}

fn write_iref(buf: &mut Vec<u8>, entries: &[([u8; 4], u32, Vec<u32>)]) {
    let p = begin_fullbox(buf, b"iref", 0, 0);
    for (ty, from, tos) in entries {
        let pe = begin_box(buf, ty);
        buf.extend_from_slice(&(*from as u16).to_be_bytes());
        buf.extend_from_slice(&(tos.len() as u16).to_be_bytes());
        for t in tos {
            buf.extend_from_slice(&(*t as u16).to_be_bytes());
        }
        end_box(buf, pe);
    }
    end_box(buf, p);
}

fn write_iprp(buf: &mut Vec<u8>, props: &[Vec<u8>], assocs: &[(u32, u16, bool)]) {
    let p = begin_box(buf, b"iprp");
    let pc = begin_box(buf, b"ipco");
    for pr in props {
        buf.extend_from_slice(pr);
    }
    end_box(buf, pc);

    let pm = begin_fullbox(buf, b"ipma", 0, 0);
    // BTreeMap so items are emitted in ascending item_id order — matches
    // our own increasing id assignment and keeps the box deterministic.
    let mut map: BTreeMap<u32, Vec<(u16, bool)>> = BTreeMap::new();
    for (id, idx, essential) in assocs {
        map.entry(*id).or_default().push((*idx, *essential));
    }
    buf.extend_from_slice(&(map.len() as u32).to_be_bytes());
    for (id, list) in &map {
        buf.extend_from_slice(&(*id as u16).to_be_bytes());
        buf.push(list.len() as u8);
        for (idx, essential) in list {
            let v = (*idx as u8 & 0x7F) | if *essential { 0x80 } else { 0 };
            buf.push(v);
        }
    }
    end_box(buf, pm);
    end_box(buf, p);
}

/// Writes `iloc` version 1 (needed for `construction_method`), 4-byte
/// offset/length fields, no `base_offset`/index fields (single extent per
/// item, always). Returns, for each `file_entries` item in order, the
/// buffer position of its 4-byte offset field paired with its `mdat`-body-
/// relative offset — [`mux`] patches those positions once `mdat`'s absolute
/// start is known.
fn write_iloc(
    buf: &mut Vec<u8>,
    file_entries: &[(u32, u64, u64)],
    idat_entries: &[(u32, u64, u64)],
) -> Vec<(usize, u64)> {
    let mut patches = Vec::new();
    let p = begin_fullbox(buf, b"iloc", 1, 0);
    buf.push(0x44); // offset_size=4, length_size=4
    buf.push(0x00); // base_offset_size=0, index_size=0
    let total = (file_entries.len() + idat_entries.len()) as u16;
    buf.extend_from_slice(&total.to_be_bytes());

    for (id, rel, len) in file_entries {
        buf.extend_from_slice(&(*id as u16).to_be_bytes());
        buf.extend_from_slice(&0u16.to_be_bytes()); // construction_method 0: file offset
        buf.extend_from_slice(&0u16.to_be_bytes()); // data_reference_index: this file
        buf.extend_from_slice(&1u16.to_be_bytes()); // extent_count
        let pos = buf.len();
        buf.extend_from_slice(&0u32.to_be_bytes()); // placeholder, patched by caller
        patches.push((pos, *rel));
        buf.extend_from_slice(&(*len as u32).to_be_bytes());
    }
    for (id, off, len) in idat_entries {
        buf.extend_from_slice(&(*id as u16).to_be_bytes());
        buf.extend_from_slice(&1u16.to_be_bytes()); // construction_method 1: idat-relative
        buf.extend_from_slice(&0u16.to_be_bytes());
        buf.extend_from_slice(&1u16.to_be_bytes());
        buf.extend_from_slice(&(*off as u32).to_be_bytes());
        buf.extend_from_slice(&(*len as u32).to_be_bytes());
    }

    end_box(buf, p);
    patches
}

fn ispe_box(width: u32, height: u32) -> Vec<u8> {
    let mut b = Vec::new();
    let p = begin_fullbox(&mut b, b"ispe", 0, 0);
    b.extend_from_slice(&width.to_be_bytes());
    b.extend_from_slice(&height.to_be_bytes());
    end_box(&mut b, p);
    b
}

fn pixi_box(channels: u8, bit_depth: u8) -> Vec<u8> {
    let mut b = Vec::new();
    let p = begin_fullbox(&mut b, b"pixi", 0, 0);
    b.push(channels);
    for _ in 0..channels {
        b.push(bit_depth);
    }
    end_box(&mut b, p);
    b
}

fn hvcc_box(payload: &[u8]) -> Vec<u8> {
    // `hvcC` is a plain Box, not a FullBox: `configurationVersion` (the
    // first byte of `payload`) takes the place a version field would.
    let mut b = Vec::new();
    let p = begin_box(&mut b, b"hvcC");
    b.extend_from_slice(payload);
    end_box(&mut b, p);
    b
}

fn auxc_box(urn: &str) -> Vec<u8> {
    let mut b = Vec::new();
    let p = begin_fullbox(&mut b, b"auxC", 0, 0);
    b.extend_from_slice(urn.as_bytes());
    b.push(0);
    end_box(&mut b, p);
    b
}

fn colr_box(c: &ColourInfo) -> Vec<u8> {
    let mut b = Vec::new();
    let p = begin_box(&mut b, b"colr"); // plain Box, not a FullBox
    match c {
        ColourInfo::Nclx { primaries, transfer, matrix, full_range } => {
            b.extend_from_slice(b"nclx");
            b.extend_from_slice(&primaries.to_be_bytes());
            b.extend_from_slice(&transfer.to_be_bytes());
            b.extend_from_slice(&matrix.to_be_bytes());
            b.push(if *full_range { 0x80 } else { 0 });
        }
        ColourInfo::Icc(bytes) => {
            // Apple's own writer uses `prof` (see docs §7); `rICC`/`prof`
            // are otherwise identical in layout, so this is a convention
            // choice, not a correctness one.
            b.extend_from_slice(b"prof");
            b.extend_from_slice(bytes);
        }
    }
    end_box(&mut b, p);
    b
}

fn clli_box(max_cll: u16, max_pall: u16) -> Vec<u8> {
    let mut b = Vec::new();
    let p = begin_box(&mut b, b"clli"); // plain Box, not a FullBox
    b.extend_from_slice(&max_cll.to_be_bytes());
    b.extend_from_slice(&max_pall.to_be_bytes());
    end_box(&mut b, p);
    b
}
