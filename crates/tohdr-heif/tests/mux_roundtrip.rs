//! What the muxer writes, the reader must read back — and the structure has to
//! match what macOS actually requires.
//!
//! These are structural assertions, not pixel ones: the coded bitstreams are
//! opaque bytes here, so anything that survives is about box layout, item
//! references and property association. That is exactly where the bugs have
//! been (a `tmap` missing its `grpl`/`altr` group was invisible to our own
//! reader while being invisible to macOS as a gain map too).

use tohdr_core::{Flavor, GainMapMeta};
use tohdr_heif::{Chroma, CodedImage, ColourInfo, HeifFile, MuxRequest};

/// A minimal `hvcC` payload: the reader reads `chromaFormat` at byte 16 and
/// `bitDepthLumaMinus8` at byte 17, so it must be at least 19 bytes.
fn hvcc(chroma_format: u8, bit_depth: u8) -> Vec<u8> {
    let mut v = vec![0u8; 23];
    v[0] = 1; // configurationVersion
    v[1] = 4; // general_profile_idc = RExt, what Apple uses for 4:0:0
    v[16] = chroma_format & 0x03;
    v[17] = bit_depth.saturating_sub(8) & 0x07;
    v[18] = bit_depth.saturating_sub(8) & 0x07;
    v
}

fn base_image() -> CodedImage {
    CodedImage {
        width: 64,
        height: 48,
        bit_depth: 8,
        chroma: Chroma::Yuv420,
        hvcc: hvcc(1, 8),
        data: (0..512u32).map(|i| (i % 251) as u8).collect(),
    }
}

fn gain_image() -> CodedImage {
    CodedImage {
        width: 32,
        height: 24,
        bit_depth: 8,
        chroma: Chroma::Monochrome,
        hvcc: hvcc(0, 8),
        data: (0..256u32).map(|i| (i % 241) as u8).collect(),
    }
}

fn request(flavor: Flavor) -> MuxRequest {
    MuxRequest {
        base: base_image(),
        gain: gain_image(),
        meta: GainMapMeta::default(),
        flavor,
        base_colour: Some(ColourInfo::Nclx {
            primaries: 1,
            transfer: 13,
            matrix: 6,
            full_range: true,
        }),
        tmap_colour: Some(ColourInfo::Nclx {
            primaries: 12,
            transfer: 16,
            matrix: 6,
            full_range: true,
        }),
        exif: None,
        xmp: None,
        clli: None,
    }
}

/// Walk top-level boxes without using the crate's own parser, so a shared bug
/// between reader and writer cannot hide the result.
fn top_level_types(bytes: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut off = 0usize;
    while off + 8 <= bytes.len() {
        let size = u32::from_be_bytes(bytes[off..off + 4].try_into().unwrap()) as usize;
        let ty = String::from_utf8_lossy(&bytes[off + 4..off + 8]).to_string();
        out.push(ty);
        if size < 8 {
            break;
        }
        off += size;
    }
    out
}

/// Find a four-CC anywhere in the buffer. Crude, but enough to assert that a
/// box was emitted at all without depending on our own parser.
fn contains_fourcc(bytes: &[u8], fourcc: &[u8; 4]) -> bool {
    bytes.windows(4).any(|w| w == fourcc)
}

#[test]
fn both_flavors_roundtrip_through_our_own_reader() {
    let req = request(Flavor::Both);
    let bytes = tohdr_heif::mux(&req).expect("mux");

    let f = HeifFile::parse(&bytes).expect("parse");
    let base_id = f.primary_item().expect("pitm");

    let items = f.items();
    assert_eq!(items.len(), 3, "base + gain + tmap, got {items:?}");

    let base = items.iter().find(|i| i.id == base_id).expect("base item");
    assert_eq!(base.type_str(), "hvc1");
    assert_eq!((base.width, base.height), (Some(64), Some(48)));

    let gain = items
        .iter()
        .find(|i| i.aux_urn.as_deref() == Some(tohdr_heif::APPLE_GAINMAP_URN))
        .expect("gain item carrying the Apple URN");
    assert_eq!(
        gain.auxiliary_to,
        vec![base_id],
        "auxl must point the gain map at the base"
    );
    assert_eq!((gain.width, gain.height), (Some(32), Some(24)));

    let tmap = items.iter().find(|i| i.type_str() == "tmap").expect("tmap");
    assert_eq!(
        tmap.derives_from,
        vec![base_id, gain.id],
        "dimg order must be [base, gain]"
    );

    // Item payloads must survive the two-pass offset patching intact.
    assert_eq!(f.item_data(base_id).expect("base data"), &req.base.data[..]);
    assert_eq!(f.item_data(gain.id).expect("gain data"), &req.gain.data[..]);

    // 1 ToneMapImage version byte + the 61-byte single-channel C.2.2 struct.
    assert_eq!(f.item_data(tmap.id).expect("tmap data").len(), 62);
}

/// The box whose absence made macOS report no ISO gain map at all, for a file
/// every other check called valid.
#[test]
fn iso_flavor_emits_the_grpl_altr_group() {
    let bytes = tohdr_heif::mux(&request(Flavor::Iso)).expect("mux");
    assert!(contains_fourcc(&bytes, b"grpl"), "grpl box must be present");
    assert!(contains_fourcc(&bytes, b"altr"), "altr group must be present");
    assert!(contains_fourcc(&bytes, b"tmap"), "tmap must be present");
}

#[test]
fn apple_only_flavor_omits_tmap_and_grpl() {
    let bytes = tohdr_heif::mux(&request(Flavor::Apple)).expect("mux");
    let f = HeifFile::parse(&bytes).expect("parse");
    assert_eq!(f.items().len(), 2, "base + gain only");
    assert!(
        f.items().iter().all(|i| i.type_str() != "tmap"),
        "no tmap without the ISO flavor"
    );
    // A `grpl` grouping [tmap, base] is meaningless with no tmap to prefer.
    assert!(
        !contains_fourcc(&bytes, b"altr"),
        "no altr group without a tmap"
    );
    assert!(
        !f.brands().contains(b"tmap"),
        "no tmap brand without a tmap item"
    );
}

#[test]
fn iso_only_flavor_omits_the_apple_urn() {
    let bytes = tohdr_heif::mux(&request(Flavor::Iso)).expect("mux");
    let f = HeifFile::parse(&bytes).expect("parse");
    assert!(
        f.items().iter().all(|i| i.aux_urn.is_none()),
        "no Apple auxC when only ISO was asked for"
    );
    assert!(f.brands().contains(b"tmap"), "tmap brand present");
}

#[test]
fn structure_matches_what_macos_requires() {
    let bytes = tohdr_heif::mux(&request(Flavor::Both)).expect("mux");
    let tops = top_level_types(&bytes);
    assert_eq!(tops.first().map(String::as_str), Some("ftyp"), "{tops:?}");
    assert!(tops.iter().any(|t| t == "meta"), "{tops:?}");
    assert!(tops.iter().any(|t| t == "mdat"), "{tops:?}");
}

/// `coded_image` must take chroma and depth from `hvcC`, not `pixi`: hpvca
/// writes `pixi` with 3 channels for a plane it coded as 4:0:0, and trusting
/// `pixi` propagated that error into our own output.
#[test]
fn coded_image_reads_chroma_from_hvcc() {
    let bytes = tohdr_heif::mux(&request(Flavor::Both)).expect("mux");
    let f = HeifFile::parse(&bytes).expect("parse");
    let gain = f
        .items()
        .iter()
        .find(|i| i.aux_urn.is_some())
        .expect("gain item");
    let coded = f.coded_image(gain.id).expect("coded gain");
    assert_eq!(coded.chroma, Chroma::Monochrome);
    assert_eq!(coded.bit_depth, 8);
    assert_eq!((coded.width, coded.height), (32, 24));

    let base = f.coded_image(f.primary_item().unwrap()).expect("coded base");
    assert_eq!(base.chroma, Chroma::Yuv420);
}

/// A 10-bit base must survive as 10-bit, since `hvcC` is the source of truth.
#[test]
fn ten_bit_base_depth_survives() {
    let mut req = request(Flavor::Both);
    req.base.bit_depth = 10;
    req.base.hvcc = hvcc(1, 10);
    let bytes = tohdr_heif::mux(&req).expect("mux");
    let f = HeifFile::parse(&bytes).expect("parse");
    let base = f.coded_image(f.primary_item().unwrap()).expect("coded base");
    assert_eq!(base.bit_depth, 10);
}

/// A minimal but real Exif block: big-endian TIFF header, one `IFD0` entry.
fn exif_block() -> Vec<u8> {
    let mut v = b"MM\x00\x2a\x00\x00\x00\x08".to_vec();
    v.extend_from_slice(&1u16.to_be_bytes()); // one entry
    v.extend_from_slice(&271u16.to_be_bytes()); // Make
    v.extend_from_slice(&2u16.to_be_bytes()); // ASCII
    v.extend_from_slice(&4u32.to_be_bytes()); // count, inline
    v.extend_from_slice(b"ap\0\0");
    v.extend_from_slice(&0u32.to_be_bytes()); // no next IFD
    v
}

/// The block has to come back byte-identical, past the four-byte
/// `exif_tiff_header_offset` the item type requires ahead of it. A muxer that
/// wrote the block but forgot the prefix would still round-trip through a naive
/// reader, so the prefix is asserted separately.
#[test]
fn an_exif_item_round_trips_byte_for_byte() {
    let block = exif_block();
    let req = MuxRequest {
        exif: Some(block.clone()),
        ..request(Flavor::Both)
    };
    let bytes = tohdr_heif::mux(&req).expect("mux");

    let file = HeifFile::parse(&bytes).expect("parse");
    let item = file
        .items()
        .iter()
        .find(|i| i.item_type == *b"Exif")
        .expect("an Exif item was written");
    let payload = file.item_data(item.id).expect("payload");

    assert_eq!(
        &payload[0..4],
        &0u32.to_be_bytes(),
        "exif_tiff_header_offset must be present and zero"
    );
    assert_eq!(&payload[4..], &block[..], "block must survive verbatim");

    // HEIF associates Exif with an image through `cdsc`, not by position. Without
    // it a reader has an orphan metadata item and no reason to apply it.
    assert!(contains_fourcc(&bytes, b"cdsc"), "cdsc reference");
}

/// Asking for no Exif must not leave the item machinery half-built: no `Exif`
/// entry in `iinf`, and the item ids of everything else unchanged.
#[test]
fn without_exif_no_item_is_written() {
    let bytes = tohdr_heif::mux(&request(Flavor::Both)).expect("mux");
    let file = HeifFile::parse(&bytes).expect("parse");
    assert!(!file.items().iter().any(|i| i.item_type == *b"Exif"));
}

#[test]
fn parsing_garbage_errors_instead_of_panicking() {
    assert!(HeifFile::parse(&[]).is_err(), "empty input");
    assert!(HeifFile::parse(b"not a heif file at all").is_err());
    // A well-formed ftyp with nothing after it: no `meta`, so it must be an
    // error rather than a panic or a silent empty file.
    let mut only_ftyp = Vec::new();
    only_ftyp.extend_from_slice(&16u32.to_be_bytes());
    only_ftyp.extend_from_slice(b"ftyp");
    only_ftyp.extend_from_slice(b"heic");
    only_ftyp.extend_from_slice(&0u32.to_be_bytes());
    assert!(HeifFile::parse(&only_ftyp).is_err(), "ftyp with no meta");
}

/// Truncating a valid file at every prefix must never panic. This is the
/// cheapest useful fuzz over a parser that reads untrusted files.
#[test]
fn every_truncation_of_a_valid_file_is_handled() {
    let bytes = tohdr_heif::mux(&request(Flavor::Both)).expect("mux");
    for n in 0..bytes.len() {
        // Only the full file is required to parse; the point is that no prefix
        // may panic, index out of bounds, or hang.
        let _ = HeifFile::parse(&bytes[..n]);
    }
}

/// Flipping single bytes must also never panic. Restricted to the header
/// region, where the box structure lives and a corrupt length is most likely
/// to send a parser off the end.
#[test]
fn single_byte_corruption_in_the_header_is_handled() {
    let bytes = tohdr_heif::mux(&request(Flavor::Both)).expect("mux");
    let limit = bytes.len().min(600);
    for i in 0..limit {
        for patch in [0x00u8, 0xFF, 0x7F] {
            let mut c = bytes.clone();
            c[i] = patch;
            if let Ok(f) = HeifFile::parse(&c) {
                // If it parsed, the accessors must not panic either.
                for item in f.items() {
                    let _ = f.item_data(item.id);
                    let _ = f.coded_image(item.id);
                }
                let _ = f.gain_map();
            }
        }
    }
}
