//! Metadata a source carried that this pipeline copies through without
//! understanding it, and the declaration of which backend can carry what.
//!
//! # Why an opaque blob is the right model
//!
//! An iPhone capture carries a `uri ` item named `metadata`, typed
//! `tag:apple.com,2023:photo:metadata:styles`, holding a 110-key binary plist of
//! Photographic Styles state and per-channel scene statistics. None of it is
//! documented, exiftool names half its keys `Tag6...`, and nothing here needs to
//! read any of it — it only needs to *not lose* it. So the item is modelled as
//! its four `infe` fields plus its bytes, and copied.
//!
//! The fields are HEIF's because HEIF is the only container this project writes.
//! They live in core rather than in `tohdr_heif` so that the encode options both
//! engines share can name them, and so an engine that cannot write them can say
//! so instead of dropping them quietly.

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
}

impl MetadataSupport {
    /// Carries nothing. What a backend gets until it says otherwise.
    pub const NONE: Self = Self {
        exif: false,
        xmp: false,
        iptc: false,
        opaque_items: false,
    };

    /// Carries everything this pipeline can hand it.
    pub const ALL: Self = Self {
        exif: true,
        xmp: true,
        iptc: true,
        opaque_items: true,
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
