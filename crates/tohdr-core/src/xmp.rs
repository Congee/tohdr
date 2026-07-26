//! The XMP copy of the gain-map headroom.
//!
//! Apple writes the headroom three times — in the ISO 21496-1 `tmap` payload,
//! in MakerApple tags 33/48, and here in XMP — and in `IMG_4913.HEIC` all three
//! agree. That redundancy is the point: different consumers read different
//! copies, so a file whose copies disagree is one where *something* will read
//! the wrong number. `DSC07752_iso.heic` dropped this copy entirely;
//! `DSC07752.heic` kept an XMP headroom of 11.863581 that its gain plane could
//! not deliver.
//!
//! # Units
//!
//! `HDRGainMapHeadroom` is **linear**, not stops — the multiplier over SDR
//! white. `IMG_4913.HEIC` carries `4.880772` alongside an ISO
//! `alternate_hdr_headroom` of `2.287109` stops, and `2^2.287109 = 4.880771`.
//! Writing stops here would understate the headroom by an exponent and is the
//! obvious way to get this wrong.

/// Apple's XMP namespace for the gain-map headroom.
pub const HDR_GAIN_MAP_NS: &str = "http://ns.apple.com/HDRGainMap/1.0/";

/// `HDRGainMapVersion` as `IMG_4913.HEIC` carries it. Packed as
/// major<<16 | minor<<8 | ..., so `131072` = `0x00020000`, which exiftool
/// renders as `0.2.0.0`. Writing `65536` instead yields `0.1.0.0` -- a
/// different version than any Apple capture declares.
pub const HDR_GAIN_MAP_VERSION: u32 = 131_072;

/// Build an XMP packet carrying `HDRGainMapHeadroom` for the given headroom in
/// **stops**; the conversion to linear happens here so callers cannot pass the
/// wrong unit by accident.
///
/// Emitted with the conventional `xpacket` wrapper and the standard
/// `W5M0MpCehiHzreSzNTczkc9d` id, which is what every XMP toolkit scans for.
pub fn headroom_packet(alt_headroom_stops: f32) -> Vec<u8> {
    let linear = alt_headroom_stops.exp2();
    format!(
        r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:HDRGainMap="{HDR_GAIN_MAP_NS}"
    HDRGainMap:HDRGainMapVersion="{HDR_GAIN_MAP_VERSION}"
    HDRGainMap:HDRGainMapHeadroom="{linear:.6}"/>
 </rdf:RDF>
</x:xmpmeta>
<?xpacket end="w"?>"#
    )
    .into_bytes()
}

/// The `rdf:Description` [`headroom_packet`] wraps, on its own.
fn headroom_description(alt_headroom_stops: f32) -> String {
    let linear = alt_headroom_stops.exp2();
    format!(
        r#"  <rdf:Description rdf:about=""
    xmlns:HDRGainMap="{HDR_GAIN_MAP_NS}"
    HDRGainMap:HDRGainMapVersion="{HDR_GAIN_MAP_VERSION}"
    HDRGainMap:HDRGainMapHeadroom="{linear:.6}"/>
"#
    )
}

/// Graft the headroom onto a source's own XMP packet, keeping everything the
/// source said.
///
/// # Why this is textual and not a parse-and-reserialize
///
/// The source's packet is the *photographer's*: keywords, title, caption,
/// rating, IPTC creator and rights, Lightroom's develop history. A round trip
/// through any partial XMP model silently drops whatever the model does not
/// cover, and no model here covers Adobe's schemas. Inserting one
/// `rdf:Description` before the closing `</rdf:RDF>` leaves every other byte
/// exactly where the source put it — a `rdf:RDF` element may hold any number of
/// descriptions, so this is well-formed by construction rather than by luck.
///
/// A source that already states `HDRGainMapHeadroom` gets ours added after it.
/// That is deliberate: the source's copy describes the source's headroom, and in
/// RDF a later property of the same subject wins, so appending states the
/// output's number without editing bytes we did not write. Callers that carry an
/// Apple gain map should be realigning the *source's* copies anyway — see
/// `tohdr_portable::align_apple_headroom` for the MakerNote half of the same
/// problem.
///
/// `None` when `source` has no `</rdf:RDF>` to insert before, i.e. is not an XMP
/// packet this function can extend; the caller should fall back to
/// [`headroom_packet`] rather than ship something malformed.
pub fn merge_headroom_into(source: &[u8], alt_headroom_stops: f32) -> Option<Vec<u8>> {
    const CLOSE: &[u8] = b"</rdf:RDF>";
    // The last one: a packet with nested RDF would otherwise get our description
    // inserted into an inner element rather than the document's own.
    let at = source
        .windows(CLOSE.len())
        .rposition(|w| w == CLOSE)?;
    let mut out = Vec::with_capacity(source.len() + 256);
    out.extend_from_slice(&source[..at]);
    out.extend_from_slice(headroom_description(alt_headroom_stops).as_bytes());
    out.extend_from_slice(&source[at..]);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packet_str(stops: f32) -> String {
        String::from_utf8(headroom_packet(stops)).expect("utf-8")
    }

    /// Pull the emitted headroom back out as a number. Asserting on the exact
    /// decimal string would make these tests fail on a last-digit rounding
    /// difference that no consumer cares about.
    fn emitted_headroom(stops: f32) -> f64 {
        let s = packet_str(stops);
        let key = "HDRGainMap:HDRGainMapHeadroom=\"";
        let start = s.find(key).expect("headroom attribute present") + key.len();
        let rest = &s[start..];
        let end = rest.find('"').expect("closing quote");
        rest[..end].parse().expect("a number")
    }

    /// The real numbers from `IMG_4913.HEIC`: 2.287109 stops alongside an XMP
    /// headroom of 4.880772. We land on 4.880770 — f32 `exp2` rounding, 2e-6
    /// away, and criterion 9 compares copies at 1e-3.
    #[test]
    fn reproduces_the_iphone_headroom() {
        let got = emitted_headroom(2.287109);
        assert!(
            (got - 4.880772).abs() < 1e-3,
            "expected ~4.880772 (what IMG_4913 carries), got {got}"
        );
    }

    /// Guards the unit mistake this module's docs warn about.
    #[test]
    fn headroom_is_linear_not_stops() {
        let got = emitted_headroom(3.0);
        assert!((got - 8.0).abs() < 1e-6, "3 stops must emit 8x, got {got}");
    }

    #[test]
    fn zero_stops_is_unity() {
        assert!((emitted_headroom(0.0) - 1.0).abs() < 1e-6);
    }

    /// exiftool renders the packed version as 0.2.0.0, matching IMG_4913.
    #[test]
    fn version_matches_what_apple_writes() {
        assert_eq!(HDR_GAIN_MAP_VERSION, 0x0002_0000);
        assert!(packet_str(1.0).contains(r#"HDRGainMapVersion="131072""#));
    }

    /// A Lightroom-shaped packet: the properties a photographer typed, which are
    /// the whole reason merging beats replacing.
    const LIGHTROOM_XMP: &str = r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/" x:xmptk="Adobe XMP Core 9.1">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:dc="http://purl.org/dc/elements/1.1/"
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmp:Rating="4">
   <dc:subject><rdf:Bag><rdf:li>sunset</rdf:li></rdf:Bag></dc:subject>
   <dc:creator><rdf:Seq><rdf:li>A Photographer</rdf:li></rdf:Seq></dc:creator>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>
<?xpacket end="w"?>"#;

    #[test]
    fn merging_keeps_every_property_the_source_stated() {
        let merged = merge_headroom_into(LIGHTROOM_XMP.as_bytes(), 2.287109).expect("mergeable");
        let s = String::from_utf8(merged).unwrap();
        for kept in [
            "xmp:Rating=\"4\"",
            "<rdf:li>sunset</rdf:li>",
            "<rdf:li>A Photographer</rdf:li>",
            "x:xmptk=\"Adobe XMP Core 9.1\"",
        ] {
            assert!(s.contains(kept), "lost {kept}");
        }
        assert!(s.contains("HDRGainMap:HDRGainMapHeadroom="));
        // Still one document with one RDF element and our description inside it.
        assert_eq!(s.matches("<rdf:RDF").count(), 1);
        assert_eq!(s.matches("</rdf:RDF>").count(), 1);
        assert!(
            s.find("HDRGainMap:HDRGainMapHeadroom=").unwrap() < s.find("</rdf:RDF>").unwrap(),
            "the headroom description must land inside rdf:RDF"
        );
        assert_eq!(s.matches("<rdf:Description").count(), 2);
    }

    /// The merged packet must still carry a readable headroom, in linear units —
    /// the same property the standalone packet is tested for.
    #[test]
    fn the_merged_headroom_is_still_linear() {
        let merged = merge_headroom_into(LIGHTROOM_XMP.as_bytes(), 3.0).unwrap();
        let s = String::from_utf8(merged).unwrap();
        let key = "HDRGainMap:HDRGainMapHeadroom=\"";
        let start = s.find(key).unwrap() + key.len();
        let v: f64 = s[start..][..s[start..].find('"').unwrap()].parse().unwrap();
        assert!((v - 8.0).abs() < 1e-6, "3 stops must merge in as 8x, got {v}");
    }

    /// Not-XMP in must not produce almost-XMP out; the caller falls back.
    #[test]
    fn something_that_is_not_a_packet_is_refused() {
        assert!(merge_headroom_into(b"", 1.0).is_none());
        assert!(merge_headroom_into(b"\x89PNG\r\n\x1a\n", 1.0).is_none());
        assert!(merge_headroom_into(b"<x:xmpmeta></x:xmpmeta>", 1.0).is_none());
    }

    /// Our own packet is mergeable too, so merging twice cannot corrupt it —
    /// which is what a second conversion of an already-converted file does.
    #[test]
    fn merging_is_idempotent_in_shape() {
        let once = merge_headroom_into(&headroom_packet(2.0), 2.0).unwrap();
        let twice = merge_headroom_into(&once, 2.0).unwrap();
        let s = String::from_utf8(twice).unwrap();
        assert_eq!(s.matches("</rdf:RDF>").count(), 1);
        assert_eq!(s.matches("<rdf:Description").count(), 3);
    }

    #[test]
    fn packet_is_well_formed_enough_to_be_scanned() {
        let s = packet_str(2.0);
        assert!(s.starts_with("<?xpacket begin="));
        assert!(s.ends_with("<?xpacket end=\"w\"?>"));
        assert!(s.contains("W5M0MpCehiHzreSzNTczkc9d"));
        assert!(s.contains(HDR_GAIN_MAP_NS));
        // Balanced tags, crudely: every element we open, we close.
        for tag in ["x:xmpmeta", "rdf:RDF"] {
            assert_eq!(s.matches(&format!("<{tag}")).count(), 1, "{tag} opened once");
            assert_eq!(s.matches(&format!("</{tag}>")).count(), 1, "{tag} closed");
        }
    }
}
