//! JPEG's `APP1` envelope for an Exif block, read and written.
//!
//! Only the container is here — nothing in this module knows what a TIFF tag
//! is. `tohdr_portable::exif` does the parsing and rebuilding; this is the byte
//! frame around it, in `tohdr-core` because both engines need it and neither
//! should depend on the other.
//!
//! Writing an `APP1` looks like a JPEG encoder's job until you see why Engine A
//! needs it: ImageIO authors that engine's whole file, and the only way to give
//! it Exif is `CGImageDestinationAddImage`'s properties dictionary — keyed by
//! `kCGImagePropertyExifDictionary` and friends, not by raw bytes. Rather than
//! hand-map every tag number onto a CF string key, Engine A wraps the block in
//! [`SMALLEST_JPEG`] and lets ImageIO parse its own way in. Measured on
//! `IMG_4913.HEIC`: a bare block yields `count=0` and no properties at all,
//! while the same block wrapped this way yields 32 Exif, 9 TIFF and 15 GPS
//! entries. See `crates/tohdr-apple/examples/probe_exif_props.rs`.

/// A valid 1x1 grayscale baseline JPEG, used only as a carrier.
///
/// It has to be a *decodable* image, not just a header: ImageIO reports
/// `count=0` for a file it cannot find an image in, and then returns no
/// properties no matter what metadata is attached.
const SMALLEST_JPEG: &[u8] = &[
    0xff, 0xd8, 0xff, 0xe0, 0x00, 0x10, 0x4a, 0x46, 0x49, 0x46, 0x00, 0x01, 0x01, 0x00, 0x00, 0x01,
    0x00, 0x01, 0x00, 0x00, 0xff, 0xdb, 0x00, 0x43, 0x00, 0x50, 0x37, 0x3c, 0x46, 0x3c, 0x32, 0x50,
    0x46, 0x41, 0x46, 0x5a, 0x55, 0x50, 0x5f, 0x78, 0xc8, 0x82, 0x78, 0x6e, 0x6e, 0x78, 0xf5, 0xaf,
    0xb9, 0x91, 0xc8, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
    0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
    0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xc0, 0x00, 0x0b, 0x08, 0x00, 0x01,
    0x00, 0x01, 0x01, 0x01, 0x11, 0x00, 0xff, 0xc4, 0x00, 0x1f, 0x00, 0x00, 0x01, 0x05, 0x01, 0x01,
    0x01, 0x01, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x02, 0x03, 0x04,
    0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0xff, 0xc4, 0x00, 0xb5, 0x10, 0x00, 0x02, 0x01, 0x03,
    0x03, 0x02, 0x04, 0x03, 0x05, 0x05, 0x04, 0x04, 0x00, 0x00, 0x01, 0x7d, 0x01, 0x02, 0x03, 0x00,
    0x04, 0x11, 0x05, 0x12, 0x21, 0x31, 0x41, 0x06, 0x13, 0x51, 0x61, 0x07, 0x22, 0x71, 0x14, 0x32,
    0x81, 0x91, 0xa1, 0x08, 0x23, 0x42, 0xb1, 0xc1, 0x15, 0x52, 0xd1, 0xf0, 0x24, 0x33, 0x62, 0x72,
    0x82, 0x09, 0x0a, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x34, 0x35,
    0x36, 0x37, 0x38, 0x39, 0x3a, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49, 0x4a, 0x53, 0x54, 0x55,
    0x56, 0x57, 0x58, 0x59, 0x5a, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6a, 0x73, 0x74, 0x75,
    0x76, 0x77, 0x78, 0x79, 0x7a, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8a, 0x92, 0x93, 0x94,
    0x95, 0x96, 0x97, 0x98, 0x99, 0x9a, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xb2,
    0xb3, 0xb4, 0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3, 0xc4, 0xc5, 0xc6, 0xc7, 0xc8, 0xc9,
    0xca, 0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda, 0xe1, 0xe2, 0xe3, 0xe4, 0xe5, 0xe6,
    0xe7, 0xe8, 0xe9, 0xea, 0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa, 0xff, 0xda,
    0x00, 0x08, 0x01, 0x01, 0x00, 0x00, 0x3f, 0x00, 0xa5, 0x5f, 0xff, 0xd9,
];

/// The six bytes that mark an `APP1` segment as Exif rather than XMP.
const EXIF_ID: &[u8] = b"Exif\0\0";

/// The identifier that marks an `APP1` segment as XMP.
pub const XMP_ID: &[u8] = b"http://ns.adobe.com/xap/1.0/\0";

/// Largest block that fits an `APP1` segment, whose length field counts itself
/// and the identifier in 16 bits.
pub const MAX_BLOCK: usize = u16::MAX as usize - 2 - EXIF_ID.len();

/// Wrap `block` in the smallest decodable JPEG that can carry it.
///
/// `None` when the block cannot fit one `APP1` segment. Splitting across
/// segments is legal for XMP and not for Exif, so there is no larger form to
/// fall back on — and no real Exif block comes close: `IMG_4913.HEIC`'s is
/// 3,074 bytes against this 65,527-byte ceiling.
pub fn wrap_in_jpeg(block: &[u8]) -> Option<Vec<u8>> {
    wrap_app1(EXIF_ID, block)
}

/// [`wrap_in_jpeg`], plus an `APP13` segment carrying an IPTC-IIM block.
///
/// # Why IPTC needs a second segment
///
/// A TIFF keeps its IIM block in `IFD0` tag `33723`, so carrying that tag carries
/// the metadata — for anything that reads TIFF. A JPEG does not: IPTC lives in
/// `APP13`'s Photoshop image-resource block, and a JPEG reader looking for IPTC
/// looks there and nowhere else. Measured: ImageIO reports 8 IPTC entries reading
/// a TIFF that has tag `33723`, and none at all from a JPEG carrying the same tag
/// inside its `APP1` — so the Exif carrier alone loses a Lightroom export's
/// creator, rights, caption and keywords on Engine A.
pub fn wrap_in_jpeg_with_iptc(block: &[u8], iptc: Option<&[u8]>) -> Option<Vec<u8>> {
    let mut out = wrap_app1(EXIF_ID, block)?;
    let Some(iim) = iptc.filter(|b| !b.is_empty()) else {
        return Some(out);
    };
    let seg = app13_iptc(iim)?;
    // After the Exif APP1, which is at a fixed position right after the SOI.
    let at = 2 + 2 + 2 + EXIF_ID.len() + block.len();
    out.splice(at..at, seg);
    Some(out)
}

/// An `APP13` segment holding `iim` as Photoshop image resource `0x0404`.
///
/// The layout is Adobe's: the `Photoshop 3.0\0` identifier, then one `8BIM`
/// resource — type, id, an empty Pascal name padded to even, the payload size,
/// and the payload padded to even. `None` if it will not fit one segment;
/// extended IPTC across segments is not a thing this needs.
fn app13_iptc(iim: &[u8]) -> Option<Vec<u8>> {
    const PS_ID: &[u8] = b"Photoshop 3.0\0";
    let pad = iim.len() % 2;
    let body_len = PS_ID.len() + 4 + 2 + 2 + 4 + iim.len() + pad;
    if body_len + 2 > u16::MAX as usize {
        return None;
    }
    let mut seg = Vec::with_capacity(body_len + 4);
    seg.extend_from_slice(&[0xff, 0xed]);
    seg.extend_from_slice(&((body_len + 2) as u16).to_be_bytes());
    seg.extend_from_slice(PS_ID);
    seg.extend_from_slice(b"8BIM");
    seg.extend_from_slice(&0x0404u16.to_be_bytes());
    seg.extend_from_slice(&[0, 0]); // empty Pascal name, padded to even
    seg.extend_from_slice(&(iim.len() as u32).to_be_bytes());
    seg.extend_from_slice(iim);
    seg.extend(core::iter::repeat_n(0u8, pad));
    Some(seg)
}

/// [`wrap_in_jpeg`] for an XMP packet rather than an Exif block.
///
/// Engine A needs this for the same reason it needs the Exif one, plus a measured
/// surprise: `CGImageMetadataCreateFromXMPData` **rejects a packet whose XML
/// attributes are single-quoted**, which is what exiftool writes by default.
/// ImageIO's file-level reader accepts the identical bytes, so handing the packet
/// to `CGImageSource` inside a carrier is not a workaround for a missing API — it
/// is the route that parses. Measured by
/// `crates/tohdr-apple/examples/probe_xmp_metadata.rs`.
pub fn wrap_xmp_in_jpeg(packet: &[u8]) -> Option<Vec<u8>> {
    wrap_app1(XMP_ID, packet)
}

fn wrap_app1(id: &[u8], block: &[u8]) -> Option<Vec<u8>> {
    if block.is_empty() || block.len() > u16::MAX as usize - 2 - id.len() {
        return None;
    }
    let mut out = Vec::with_capacity(SMALLEST_JPEG.len() + block.len() + 10);
    // Straight after the SOI, which is where every reader expects it.
    out.extend_from_slice(&SMALLEST_JPEG[..2]);
    out.extend_from_slice(&[0xff, 0xe1]);
    out.extend_from_slice(&((2 + id.len() + block.len()) as u16).to_be_bytes());
    out.extend_from_slice(id);
    out.extend_from_slice(block);
    out.extend_from_slice(&SMALLEST_JPEG[2..]);
    Some(out)
}

/// The payload of the first `Exif\0\0`-tagged `APP1` segment in a JPEG.
pub fn app1_payload(bytes: &[u8]) -> Option<&[u8]> {
    app1_segments(bytes).find_map(|s| s.strip_prefix(EXIF_ID))
}

/// Every `APP1` segment body in a JPEG, in file order, identifier included.
///
/// `APP1` is shared: Exif takes the ones starting `Exif\0\0` and XMP the ones
/// starting with Adobe's namespace URI, so a caller has to see the identifier to
/// know which is which. Walking stops at the first thing that is not a marker
/// segment — a truncated file yields what it had rather than nothing.
pub fn app1_segments(bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
    segments(bytes).filter_map(|(m, b)| (m == 0xe1).then_some(b))
}

/// The IPTC-IIM payload of a JPEG's `APP13` Photoshop resource block.
///
/// A JPEG keeps IPTC here, not in its Exif IFD, so a source read for Exif alone
/// loses a photographer's creator, rights and keywords entirely. The mirror of
/// [`wrap_in_jpeg_with_iptc`], and the reason this module writes that segment at
/// all: the same information has to travel in both directions.
pub fn app13_iptc_payload(bytes: &[u8]) -> Option<&[u8]> {
    const PS_ID: &[u8] = b"Photoshop 3.0\0";
    for (marker, body) in segments(bytes) {
        if marker != 0xed {
            continue;
        }
        let mut at = body.strip_prefix(PS_ID)?;
        // A run of `8BIM` resources: type, 16-bit id, a Pascal name padded to
        // even, a 32-bit size, then the payload padded to even.
        while at.len() >= 12 {
            let (head, rest) = at.split_at(4);
            if head != b"8BIM" {
                break;
            }
            let id = u16::from_be_bytes([rest[0], rest[1]]);
            let name_len = rest[2] as usize;
            // The length byte is part of the Pascal string, so the pair is
            // padded together.
            let name_total = (1 + name_len) + (1 + name_len) % 2;
            let rest = rest.get(2 + name_total..)?;
            let size = u32::from_be_bytes([rest[0], rest[1], rest[2], rest[3]]) as usize;
            let payload = rest.get(4..4 + size)?;
            if id == 0x0404 && !payload.is_empty() {
                return Some(payload);
            }
            at = rest.get(4 + size + size % 2..)?;
        }
    }
    None
}

/// Every marker segment's `(marker, body)`, in file order.
fn segments(bytes: &[u8]) -> impl Iterator<Item = (u8, &[u8])> {
    let mut at = if bytes.len() >= 4 && bytes[0..2] == [0xff, 0xd8] {
        Some(2usize)
    } else {
        None
    };
    core::iter::from_fn(move || {
        loop {
            let mut p = at?;
            // Segments are `ff <marker> <u16 length, counting itself>`. Fill
            // bytes of `ff` before a marker are legal and must be skipped.
            while bytes.get(p) == Some(&0xff) && bytes.get(p + 1) == Some(&0xff) {
                p += 1;
            }
            if bytes.get(p) != Some(&0xff) {
                at = None;
                return None;
            }
            let marker = *bytes.get(p + 1)?;
            // Start of scan or end of image: no headers remain.
            if marker == 0xda || marker == 0xd9 {
                at = None;
                return None;
            }
            let len = u16::from_be_bytes([*bytes.get(p + 2)?, *bytes.get(p + 3)?]) as usize;
            if len < 2 {
                at = None;
                return None;
            }
            let body = bytes.get(p + 4..p + 2 + len);
            at = Some(p + 2 + len);
            match body {
                None => {
                    at = None;
                    return None;
                }
                Some(b) => return Some((marker, b)),
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `APP13` layout is Adobe's and a reader will reject a wrong length or a
    /// missing pad, so the bytes are checked field by field rather than by
    /// round-tripping through our own parser.
    #[test]
    fn the_iptc_segment_matches_adobes_layout() {
        // Odd length on purpose: the payload must be padded to even and the
        // segment length must count the pad.
        let iim = b"\x1c\x02\x19\x00\x06sunset";
        assert_eq!(iim.len() % 2, 1);
        let jpeg = wrap_in_jpeg_with_iptc(b"MM\0\x2a\0\0\0\x08", Some(iim)).expect("fits");

        let at = jpeg
            .windows(2)
            .position(|w| w == [0xff, 0xed])
            .expect("an APP13 segment");
        let len = u16::from_be_bytes([jpeg[at + 2], jpeg[at + 3]]) as usize;
        let body = &jpeg[at + 4..at + 2 + len];
        assert_eq!(len % 2, 0, "segment length must be even");
        assert!(body.starts_with(b"Photoshop 3.0\0"));
        let res = &body[14..];
        assert_eq!(&res[0..4], b"8BIM");
        assert_eq!(u16::from_be_bytes([res[4], res[5]]), 0x0404);
        assert_eq!(&res[6..8], &[0, 0], "empty Pascal name, padded");
        assert_eq!(
            u32::from_be_bytes([res[8], res[9], res[10], res[11]]) as usize,
            iim.len(),
            "declared size is the unpadded length"
        );
        assert_eq!(&res[12..12 + iim.len()], iim);
        assert_eq!(res.len(), 12 + iim.len() + 1, "one pad byte, counted");

        // And the Exif block is still where it was, ahead of the new segment.
        assert_eq!(app1_payload(&jpeg), Some(&b"MM\0\x2a\0\0\0\x08"[..]));
    }

    /// No IPTC must produce the same bytes as the plain wrapper, so adding the
    /// parameter cannot have changed the Exif path.
    #[test]
    fn no_iptc_is_byte_identical_to_the_plain_wrapper() {
        let block = b"MM\0\x2a\0\0\0\x08\0\0\0\0";
        assert_eq!(
            wrap_in_jpeg_with_iptc(block, None),
            wrap_in_jpeg(block),
            "the no-IPTC path must not differ"
        );
        assert_eq!(
            wrap_in_jpeg_with_iptc(block, Some(&[])),
            wrap_in_jpeg(block),
            "an empty block is the same as none"
        );
    }

    /// An XMP carrier is the same envelope with Adobe's identifier, and
    /// `app1_segments` has to see both segments when both are present.
    #[test]
    fn an_xmp_carrier_is_tagged_with_adobes_namespace() {
        let packet = b"<x:xmpmeta><rdf:RDF/></x:xmpmeta>";
        let jpeg = wrap_xmp_in_jpeg(packet).expect("fits");
        let seg = app1_segments(&jpeg).next().expect("one APP1");
        assert_eq!(seg.strip_prefix(XMP_ID), Some(&packet[..]));
        // Not mistakable for Exif, which shares the marker.
        assert_eq!(app1_payload(&jpeg), None);
    }

    #[test]
    fn wrap_then_read_returns_the_block() {
        let block = b"MM\0\x2a\0\0\0\x08\0\0\0\0".to_vec();
        let jpeg = wrap_in_jpeg(&block).expect("fits");
        assert_eq!(app1_payload(&jpeg), Some(&block[..]));
    }

    /// The carrier has to stay a valid JPEG, or ImageIO finds no image in it and
    /// returns no properties — the whole point of the wrapper.
    #[test]
    fn the_carrier_keeps_its_markers() {
        let jpeg = wrap_in_jpeg(b"MM\0\x2a\0\0\0\x08").unwrap();
        assert_eq!(&jpeg[0..2], &[0xff, 0xd8], "SOI");
        assert_eq!(&jpeg[jpeg.len() - 2..], &[0xff, 0xd9], "EOI");
        // Every byte of the original carrier is still present, in order.
        assert!(jpeg.windows(4).any(|w| w == [0xff, 0xc0, 0x00, 0x0b]), "SOF0");
        assert!(jpeg.windows(2).any(|w| w == [0xff, 0xda]), "SOS");
    }

    #[test]
    fn oversize_and_empty_blocks_are_refused() {
        assert!(wrap_in_jpeg(b"").is_none());
        assert!(wrap_in_jpeg(&vec![0u8; MAX_BLOCK]).is_some());
        assert!(wrap_in_jpeg(&vec![0u8; MAX_BLOCK + 1]).is_none());
    }

    #[test]
    fn a_jpeg_without_exif_yields_none() {
        assert_eq!(app1_payload(SMALLEST_JPEG), None);
        assert_eq!(app1_payload(b""), None);
        assert_eq!(app1_payload(b"MM\0\x2a"), None);
    }

    /// An `APP1` that is XMP, not Exif, must not be mistaken for one.
    #[test]
    fn xmp_app1_is_not_exif() {
        let mut j = vec![0xff, 0xd8, 0xff, 0xe1];
        let id = b"http://ns.adobe.com/xap/1.0/\0";
        j.extend_from_slice(&((2 + id.len()) as u16).to_be_bytes());
        j.extend_from_slice(id);
        j.extend_from_slice(&[0xff, 0xda, 0x00, 0x02]);
        assert_eq!(app1_payload(&j), None);
    }
}
