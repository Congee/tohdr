//! Metadata a source carried that this pipeline copies through without
//! understanding it, and the declaration of which backend can carry what.
//!
//! An opaque blob is the right model because there is nothing to read: an iPhone
//! capture's `uri ` item holds a 110-key undocumented binary plist of
//! Photographic Styles state, and this pipeline only needs to *not lose* it. So an
//! item is its four `infe` fields plus its bytes.
//!
//! Those fields are HEIF's because HEIF is the only container written here. They
//! live in core so the shared encode options can name them, and so an engine that
//! cannot write them says so instead of dropping them quietly.

/// One non-image item, copied byte for byte.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpaqueItem {
    /// The four-character `infe` item type: `uri `, `mime`, and so on. Never an
    /// image type — an image item would need its own coded data, `hvcC` and
    /// `ispe`, which is a different job than copying metadata.
    pub item_type: [u8; 4],
    /// `infe`'s `item_name`. `"metadata"` for Apple's styles item, empty for
    /// most things.
    pub name: String,
    /// `infe`'s `content_type`, present exactly when `item_type` is `mime`.
    pub content_type: Option<String>,
    /// `infe`'s `item_uri_type`, present exactly when `item_type` is `uri `.
    /// Dropping it would leave a reader unable to tell what the bytes are.
    pub uri_type: Option<String>,
    /// `infe`'s hidden flag, as the source set it.
    pub hidden: bool,
    pub data: Vec<u8>,
}

impl OpaqueItem {
    /// A short description for a progress line: `uri:metadata` or `mime`.
    pub fn label(&self) -> String {
        let ty = core::str::from_utf8(&self.item_type)
            .unwrap_or("????")
            .trim_end();
        if self.name.is_empty() {
            ty.to_string()
        } else {
            format!("{ty}:{}", self.name)
        }
    }
}

/// Which kinds of source metadata a backend actually writes.
///
/// Each field is a claim the backend makes about its own output, defaulting to
/// `false`. That direction matters: a caller can then tell the user what was
/// dropped, where a silently-ignored options field looks identical to a source
/// that never had the metadata.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MetadataSupport {
    /// Writes [`crate::EncodeOptions::exif`].
    pub exif: bool,
    /// Writes [`crate::EncodeOptions::xmp`] — the source's packet, not just our
    /// own headroom one.
    pub xmp: bool,
    /// Writes the IPTC-IIM block. Separate from `exif` because it can be handed
    /// to a backend *inside* the Exif block and still not reach the output: macOS
    /// ImageIO reads IPTC from a carrier and then writes no IPTC into a HEIC, so
    /// Engine A claims `exif` and not this.
    pub iptc: bool,
    /// Writes [`crate::EncodeOptions::opaque_items`].
    pub opaque_items: bool,
    /// Writes a `MakerNote` of any vendor, not just Apple's.
    ///
    /// False for a backend that reaches its writer through *parsed* metadata
    /// rather than the block's bytes. macOS ImageIO has a property key for
    /// Apple's `MakerNote` and none for a raw vendor blob, so Engine A carries
    /// Apple's 25 tags and loses everyone else's in translation. Measured on a
    /// HEIC whose Exif block holds a byte-identical 38,332-byte Sony blob:
    /// `exiftool -MakerNotes:all` reports 0 tags out of Engine A and 124 out of
    /// Engine B. Separate from `exif` for the same reason `iptc` is — the block
    /// arrives complete and part of it still does not reach the file.
    pub maker_note: bool,
    /// Largest Exif block this backend can write, when it has a limit.
    ///
    /// `None` means the block goes into the output whole, at any size — a HEIF
    /// `Exif` item's length is a 32-bit box size. A backend that hands the block
    /// to a JPEG carrier is capped by that carrier's 16-bit `APP1` length
    /// instead, and answers an oversize block by writing *no* metadata at all,
    /// so the caller has to know the ceiling to keep that from happening
    /// silently. Only a grafted vendor `MakerNote` makes a photograph's block
    /// approach it; see `tohdr_portable::exif`.
    pub max_exif_block: Option<usize>,
}

impl MetadataSupport {
    /// Carries nothing. What a backend gets until it says otherwise.
    pub const NONE: Self = Self {
        exif: false,
        xmp: false,
        iptc: false,
        opaque_items: false,
        maker_note: false,
        max_exif_block: None,
    };

    /// Carries everything this pipeline can hand it, at any size.
    pub const ALL: Self = Self {
        exif: true,
        xmp: true,
        iptc: true,
        opaque_items: true,
        maker_note: true,
        max_exif_block: None,
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_read_like_the_item_they_name() {
        let apple = OpaqueItem {
            item_type: *b"uri ",
            name: "metadata".into(),
            content_type: None,
            uri_type: Some("tag:apple.com,2023:photo:metadata:styles".into()),
            hidden: true,
            data: vec![1, 2, 3],
        };
        assert_eq!(apple.label(), "uri:metadata");
        assert_eq!(
            OpaqueItem {
                name: String::new(),
                ..apple
            }
            .label(),
            "uri"
        );
    }

    #[test]
    fn nothing_is_supported_by_default() {
        assert_eq!(MetadataSupport::default(), MetadataSupport::NONE);
        assert!(!MetadataSupport::NONE.exif);
        assert!(MetadataSupport::ALL.exif && MetadataSupport::ALL.xmp);
    }
}
