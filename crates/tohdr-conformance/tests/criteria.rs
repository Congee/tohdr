//! Every criterion must fail on an input that breaks it.
//!
//! A checker that only ever sees good files is untested: two of the criteria in
//! `docs/acceptance-criteria.md` were once tautologies that could not fail, and
//! nothing caught that until a file they should have rejected passed. So each
//! test here takes the committed good file, breaks exactly one thing in place,
//! and asserts the verdict moves.
//!
//! `include_bytes!` rather than a path read: no I/O, and the fixture cannot go
//! missing at runtime.

use tohdr_conformance::{analyze, check, Flavor, Status};
use ultrahdr_core::AppleHdrInfo;

/// A 512x384 both-flavor conversion of a synthetic HDR source, written by
/// `--engine hpvca` so it is reproducible without the media block.
const GOOD: &[u8] = include_bytes!("../../../assets/fixtures/conformance_both.heic");

/// Offsets within the 62-byte ISO 21496-1 payload (1 version byte + the 61-byte
/// single-channel C.2.2 struct), which the fixture stores in `idat`.
mod iso {
    pub const BASE_HEADROOM_NUM: usize = 6;
    pub const ALT_HEADROOM_NUM: usize = 14;
    pub const GAMMA_DEN: usize = 42;
}

fn find(hay: &[u8], needle: &[u8]) -> usize {
    hay.windows(needle.len())
        .position(|w| w == needle)
        .unwrap_or_else(|| panic!("fixture has no {:?}", String::from_utf8_lossy(needle)))
}

/// Start of the tone-map payload: the `idat` box body, 8 bytes past the box start
/// and so 4 past its type.
fn iso_payload(bytes: &[u8]) -> usize {
    find(bytes, b"idat") + 4
}

/// The good file with `patch` applied at `at`. Length never changes, so every
/// offset elsewhere in the container stays valid and only one thing is wrong.
fn patched(at: usize, patch: &[u8]) -> Vec<u8> {
    let mut out = GOOD.to_vec();
    out[at..at + patch.len()].copy_from_slice(patch);
    out
}

/// The status of one criterion, or a panic naming what was reported instead.
fn status_of(bytes: &[u8], criterion: u32, expect: Option<Flavor>) -> Status {
    let info = analyze(bytes).expect("analyze");
    let checks = check(&info, expect);
    checks
        .iter()
        .find(|c| c.criterion == criterion)
        .map(|c| c.status)
        .unwrap_or_else(|| panic!("criterion {criterion} was not reported at all"))
}

fn fails(bytes: &[u8], criterion: u32) {
    let s = status_of(bytes, criterion, Some(Flavor::Both));
    assert_eq!(s, Status::Fail, "criterion {criterion} should have failed, got {s:?}");
}

#[test]
fn the_good_file_passes_every_applicable_criterion() {
    let info = analyze(GOOD).expect("analyze");
    let checks = check(&info, Some(Flavor::Both));
    let failed: Vec<_> = checks
        .iter()
        .filter(|c| c.status == Status::Fail)
        .map(|c| format!("{}: {}", c.criterion, c.detail))
        .collect();
    assert!(failed.is_empty(), "unexpected failures: {failed:?}");

    // 8 is the only skip: a TIFF source has no MakerNote to carry, so there are
    // no MakerApple tags to judge. Skipping is the correct answer, not a gap.
    let skipped: Vec<u32> =
        checks.iter().filter(|c| c.status == Status::Skip).map(|c| c.criterion).collect();
    assert_eq!(skipped, vec![8]);
}

#[test]
fn criterion_1_fails_when_pitm_names_no_item() {
    // pitm body is version/flags then a u16 item id.
    let at = find(GOOD, b"pitm") + 4 + 4;
    fails(&patched(at, &[0x00, 0x63]), 1);
}

#[test]
fn criterion_2_fails_on_a_10_bit_gain_plane() {
    // The single-channel pixi: version/flags, one channel, 8 bits.
    let at = find(GOOD, b"pixi\x00\x00\x00\x00\x01\x08") + 9;
    fails(&patched(at, &[10]), 2);
}

#[test]
fn criterion_3_fails_when_the_apple_urn_is_wrong() {
    let at = find(GOOD, tohdr_conformance::APPLE_URN.as_bytes());
    // `hdrgainmap` -> `hdrgainmaq`: still a URN, no longer Apple's.
    fails(&patched(at + tohdr_conformance::APPLE_URN.len() - 1, b"q"), 3);
}

#[test]
fn criterion_4_fails_without_the_tmap_brand() {
    // The first `tmap` in the file is the ftyp brand; the item type comes later.
    fails(&patched(find(GOOD, b"tmap"), b"xmap"), 4);
}

#[test]
fn criterion_5_fails_on_a_headroom_declared_either_way_off() {
    let at = iso_payload(GOOD) + iso::ALT_HEADROOM_NUM;
    let num = u32::from_be_bytes(GOOD[at..at + 4].try_into().unwrap());
    for (what, declared) in [("under", num / 2), ("over", num * 2)] {
        // The plane still encodes what it encoded; only the declaration moves.
        // Over-declaring is the ISO-flavor export's actual defect -- 3.568 stops
        // declared against 1.96 encoded -- and makes a renderer *under*-apply
        // the map, which is criterion 10.
        let broken = patched(at, &declared.to_be_bytes());
        assert_eq!(status_of(&broken, 5, Some(Flavor::Both)), Status::Fail, "{what}-declared");
        assert_eq!(status_of(&broken, 10, Some(Flavor::Both)), Status::Fail, "{what}-declared");
    }
}

#[test]
fn criterion_4_fails_when_the_payload_lies_about_its_channel_count() {
    // Set is_multichannel on a 62-byte single-channel payload: three channels
    // need 142 bytes, so the length no longer matches what the file declares.
    let at = iso_payload(GOOD) + 5;
    fails(&patched(at, &[GOOD[at] | 0x80]), 4);
}

#[test]
fn the_payload_length_rule_holds_for_real_three_channel_bytes() {
    // No writer here emits a 3-channel gain map, so the bytes come from
    // ultrahdr-core rather than from a file -- but they are real ISO 21496-1
    // payloads, which is what criterion 4 measures.
    use ultrahdr_core::{serialize_iso21496_fmt, GainMapMetadata, Iso21496Format::AvifTmap};

    let mut one = GainMapMetadata::default();
    one.channels = [one.channels[0]; 3];
    let bytes = serialize_iso21496_fmt(&one, AvifTmap);
    assert_eq!(bytes.len(), 62);
    assert_eq!(tohdr_conformance::expected_payload_len(&bytes), 62);

    let mut three = one;
    three.channels[1].max = 1.5; // differing channels force the 3-channel form
    let bytes = serialize_iso21496_fmt(&three, AvifTmap);
    assert_eq!(bytes.len(), 142, "3-channel payload should be 1 + 141 bytes");
    assert_eq!(tohdr_conformance::expected_payload_len(&bytes), 142);
}

#[test]
fn criterion_0_fails_when_neither_flavor_is_signaled() {
    // Break the URN and the tmap item type: a HEIC with a gain-map-shaped
    // auxiliary image and nothing that says what it is.
    let no_urn = patched(find(GOOD, tohdr_conformance::APPLE_URN.as_bytes()) + 4, b"x");
    // The first `tmap` is the ftyp brand, the second is the item type.
    let brand = find(&no_urn, b"tmap") + 4;
    let item_type = brand + find(&no_urn[brand..], b"tmap");
    let mut blind = no_urn;
    blind[item_type..item_type + 4].copy_from_slice(b"xmap");
    let info = analyze(&blind).expect("analyze");
    let zero = check(&info, None).into_iter().find(|c| c.criterion == 0).expect("criterion 0");
    assert_eq!(zero.status, Status::Fail, "{}", zero.detail);
}

#[test]
fn criterion_6_fails_when_the_base_declares_headroom() {
    // Numerator = denominator = 1: an SDR base claiming one stop of headroom.
    let at = iso_payload(GOOD) + iso::BASE_HEADROOM_NUM;
    fails(&patched(at, &[0, 0, 0, 1, 0, 0, 0, 1]), 6);
}

#[test]
fn criterion_7_fails_on_a_zero_denominator() {
    // A zero gamma denominator is what avifGainMapValidateMetadata rejects
    // first, and what `parse_iso21496_fmt` refuses to hand back at all.
    let broken = patched(iso_payload(GOOD) + iso::GAMMA_DEN, &[0, 0, 0, 0]);
    fails(&broken, 7);
    // Deliberately *not* criterion 4: the item, its references, its brand and
    // its length are all still right, and a metadata defect that failed the
    // structural criterion would leave 7 unable to fail on the one thing it
    // names. The retired Python checker attributed this to 4 and skipped 5, 6, 7.
    assert_eq!(status_of(&broken, 4, Some(Flavor::Both)), Status::Pass);
    // An ISO payload that will not parse is a failure of the criteria that read
    // it, never a skip -- a skip here is how a criterion stops being able to fail.
    for n in [5, 6, 10] {
        fails(&broken, n);
    }
}

#[test]
fn criterion_8_judges_the_tags_it_is_given() {
    let tags = |t33: Option<f64>, t48: f64| AppleHdrInfo {
        hdr_headroom: t33,
        hdr_gain: t48,
        hdr_image_type: Some(3),
    };
    let status = |i: &AppleHdrInfo| tohdr_conformance::check_apple_tags(Some(i)).0;

    // The Apple-flavor export's actual defect: tag 48 out of domain.
    assert_eq!(status(&tags(Some(1.0), -0.008_120_966_145)), Status::Fail);
    assert_eq!(status(&tags(Some(-1.0), 0.05)), Status::Fail, "tag 33 selects no branch");
    assert_eq!(status(&tags(None, 0.05)), Status::Fail, "tag 48 with no tag 33");
    // The reference capture's tags.
    assert_eq!(status(&tags(Some(1.01), 0.052_539_076_655)), Status::Pass);
    // Nothing written at all is a skip, not a failure: there is no tag to judge.
    assert_eq!(tohdr_conformance::check_apple_tags(None).0, Status::Skip);
    assert_eq!(status(&tags(None, 0.0)), Status::Skip);
}

#[test]
fn criterion_9_fails_when_the_xmp_copy_disagrees() {
    // Same length, so only the value changes.
    let at = find(GOOD, b"HDRGainMapHeadroom=\"") + b"HDRGainMapHeadroom=\"".len();
    fails(&patched(at, b"1."), 9);
}

#[test]
fn criterion_17_fails_without_the_altr_group() {
    // The defect only macOS ImageIO caught: every box correct, no entity group,
    // and no ISO gain map reported at all.
    fails(&patched(find(GOOD, b"altr"), b"xltr"), 17);
}

#[test]
fn an_absent_flavor_fails_when_expected_and_skips_when_not() {
    let no_urn = patched(
        find(GOOD, tohdr_conformance::APPLE_URN.as_bytes()) + 4,
        b"x",
    );
    // Asked for it: its absence is the answer.
    assert_eq!(status_of(&no_urn, 3, Some(Flavor::Both)), Status::Fail);
    // Asked for ISO only, and with no expectation at all: still reported, as a
    // skip. A criterion that disappears from the report reads as one nobody
    // thought about.
    assert_eq!(status_of(&no_urn, 3, Some(Flavor::Iso)), Status::Skip);
    assert_eq!(status_of(&no_urn, 3, None), Status::Skip);
}

#[test]
fn the_apple_headroom_formula_matches_the_reference_capture() {
    // The reference capture's own three copies agree, which is what pins these
    // coefficients: its MakerApple tags must land on the 2.287109 stops its ISO
    // payload and XMP both declare.
    let stops = tohdr_conformance::apple_headroom_stops(1.009_999_990_462_574_7, 0.052_539_076_655);
    assert!((stops - 2.287_109).abs() < 1e-4, "{stops} stops, want 2.287109");

    // And the Apple-flavor export's negative tag 48 lands on the 3.568 stops
    // that is above what the formula can express -- the reason it went negative.
    let broken = tohdr_conformance::apple_headroom_stops(1.0, -0.008_120_966_145);
    assert!((broken - 3.568_47).abs() < 1e-4, "{broken} stops, want 3.56847");
}

#[test]
fn libavif_weight_including_the_branch_that_returns_zero() {
    use tohdr_conformance::gain_weight;
    // Equal headrooms: libavif declines to apply the map at all.
    assert_eq!(gain_weight(2.0, 2.0, 5.0), 0.0);
    assert_eq!(gain_weight(0.0, 4.0, 2.0), 0.5);
    assert_eq!(gain_weight(0.0, 4.0, 9.0), 1.0, "clamped, not extrapolated");
    // Alternate darker than base: the only path that returns a negative weight.
    assert_eq!(gain_weight(4.0, 0.0, 2.0), -0.5);
    assert_eq!(gain_weight(4.0, 0.0, 6.0), 0.0);
}

#[test]
fn garbage_and_truncation_are_errors_rather_than_panics() {
    assert!(analyze(&[]).is_err());
    assert!(analyze(b"not a heic at all").is_err());
    // Every prefix of a real file: a truncated download must not panic. Some
    // prefixes parse (the boxes that survive), which is fine -- the criteria
    // then fail on what is missing.
    for n in (0..GOOD.len()).step_by(7) {
        let _ = analyze(&GOOD[..n]).map(|i| check(&i, None));
    }
    // And a byte-level fuzz of the header region, where the box sizes live.
    let mut state = 0x2545_f491_4f6c_dd1du64;
    for _ in 0..2000 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let at = (state as usize) % 2048;
        let mut bytes = GOOD.to_vec();
        bytes[at] ^= (state >> 32) as u8;
        let _ = analyze(&bytes).map(|i| check(&i, None));
    }
}
