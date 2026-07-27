//! Everything a source says about the photograph that is neither pixels nor Exif:
//! its XMP packet, and any opaque item that describes it.
//!
//! HEIF answers "about the photograph" in the file itself: a `cdsc` reference
//! means *this item describes that one*. In the reference capture the Exif item and the
//! Photographic Styles plist point at the primary image and its `tmap`, while four
//! XMP items point at auxiliary images (sky, skin and portrait mattes, and the
//! gain map). Carrying those four forward would claim mattes the output does not
//! have, so the `cdsc` target *is* the filter -- an item is carried only if it
//! describes the primary.
//!
//! Not a heuristic for a missing rule; it is the rule the container defines.

use std::path::Path;

use tohdr_core::OpaqueItem;

use crate::Result;

/// Item types that carry coded pixels, or derive from items that do. Copying one
/// means copying a bitstream plus its `hvcC`, `ispe` and `pixi` — a different
/// job from copying metadata, and one this function does not attempt.
const IMAGE_TYPES: &[&[u8; 4]] = &[b"hvc1", b"hvc2", b"av01", b"grid", b"iovl", b"iden", b"tmap"];

/// The MIME type an XMP packet is stored under in a HEIF `mime` item.
const XMP_CONTENT_TYPE: &str = "application/rdf+xml";

/// What a source carried alongside its pixels.
#[derive(Clone, Debug, Default)]
pub struct Sidecar {
    /// The source's XMP packet — keywords, title, caption, rating, IPTC creator
    /// and rights, develop history. `None` when the source has none.
    pub xmp: Option<Vec<u8>>,
    /// Opaque items describing the photograph, to be copied verbatim.
    pub items: Vec<OpaqueItem>,
}

impl Sidecar {
    pub fn is_empty(&self) -> bool {
        self.xmp.is_none() && self.items.is_empty()
    }

    /// Total bytes, for a progress line: these are copied whole, and Apple's
    /// plist alone is a few KB.
    pub fn bytes(&self) -> usize {
        self.xmp.as_ref().map_or(0, Vec::len) + self.items.iter().map(|i| i.data.len()).sum::<usize>()
    }
}

/// Read `path`'s XMP and describing items.
///
/// Never an error for a file that simply has none, and never fatal for one this
/// reader cannot parse: metadata is not a reason to refuse a photograph.
pub fn read(path: &Path) -> Result<Sidecar> {
    let bytes = std::fs::read(path)?;
    Ok(read_bytes(&bytes))
}

/// [`read`] on an in-memory file, so tests need no filesystem.
pub fn read_bytes(bytes: &[u8]) -> Sidecar {
    if bytes.len() < 12 {
        return Sidecar::default();
    }
    if bytes[0..2] == [0xff, 0xd8] {
        return Sidecar {
            xmp: jpeg_xmp(bytes).map(<[u8]>::to_vec),
            items: Vec::new(),
        };
    }
    if &bytes[4..8] == b"ftyp" {
        return heif_sidecar(bytes);
    }
    if matches!(&bytes[0..2], b"II" | b"MM") {
        return Sidecar {
            xmp: crate::gainmap_tiff::ifd0_xmp(bytes),
            items: Vec::new(),
        };
    }
    Sidecar::default()
}

/// The XMP `APP1` segment of a JPEG, which is the one tagged with Adobe's
/// namespace URI rather than `Exif\0\0`.
fn jpeg_xmp(bytes: &[u8]) -> Option<&[u8]> {
    const XMP_ID: &[u8] = b"http://ns.adobe.com/xap/1.0/\0";
    tohdr_core::exif::app1_segments(bytes).find_map(|seg| seg.strip_prefix(XMP_ID))
}

/// A HEIF source's XMP packet and its describing items.
fn heif_sidecar(bytes: &[u8]) -> Sidecar {
    let Ok(file) = tohdr_heif::HeifFile::parse(bytes) else {
        return Sidecar::default();
    };
    // With no primary item there is nothing for an item to describe, and
    // "describes the photograph" is the only test that keeps a matte's metadata
    // out of the output.
    let Some(primary) = file.primary_item() else {
        return Sidecar::default();
    };

    let mut out = Sidecar::default();
    for item in file.items() {
        if IMAGE_TYPES.contains(&&item.item_type) || item.item_type == *b"Exif" {
            continue;
        }
        if !item.describes.contains(&primary) {
            continue;
        }
        let Ok(data) = file.item_data(item.id) else {
            continue;
        };
        if data.is_empty() {
            continue;
        }
        if item.content_type.as_deref() == Some(XMP_CONTENT_TYPE) {
            // One XMP packet per file: a second `mime` item claiming to describe
            // the same image is not something any writer produces, and merging
            // two RDF documents is not a thing to guess at.
            if out.xmp.is_none() {
                out.xmp = Some(data.to_vec());
            }
            continue;
        }
        out.items.push(OpaqueItem {
            item_type: item.item_type,
            name: item.name.clone(),
            content_type: item.content_type.clone(),
            uri_type: item.uri_type.clone(),
            hidden: item.hidden,
            data: data.to_vec(),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_in_nothing_out() {
        assert!(read_bytes(b"").is_empty());
        assert!(read_bytes(b"not an image at all").is_empty());
        assert!(read_bytes(&[0xff, 0xd8, 0xff, 0xd9]).is_empty());
    }

    #[test]
    fn a_jpeg_xmp_segment_is_found_past_its_namespace_id() {
        let packet = b"<x:xmpmeta>keywords</x:xmpmeta>";
        let mut seg = b"http://ns.adobe.com/xap/1.0/\0".to_vec();
        seg.extend_from_slice(packet);
        let jpeg = jpeg_with_app1(&[b"Exif\0\0somewhere-else".to_vec(), seg]);
        let got = read_bytes(&jpeg);
        assert_eq!(got.xmp.as_deref(), Some(&packet[..]));
        assert!(got.items.is_empty());
    }

    /// A JPEG with the given `APP1` payloads, then an empty scan.
    fn jpeg_with_app1(payloads: &[Vec<u8>]) -> Vec<u8> {
        let mut out = vec![0xff, 0xd8];
        for p in payloads {
            out.extend_from_slice(&[0xff, 0xe1]);
            out.extend_from_slice(&((p.len() + 2) as u16).to_be_bytes());
            out.extend_from_slice(p);
        }
        out.extend_from_slice(&[0xff, 0xda, 0x00, 0x02, 0xff, 0xd9]);
        out
    }

    #[test]
    fn an_item_bytes_count_covers_both_halves() {
        let s = Sidecar {
            xmp: Some(vec![0; 10]),
            items: vec![OpaqueItem {
                item_type: *b"uri ",
                name: "metadata".into(),
                content_type: None,
                uri_type: None,
                hidden: true,
                data: vec![0; 32],
            }],
        };
        assert_eq!(s.bytes(), 42);
        assert!(!s.is_empty());
    }
}
