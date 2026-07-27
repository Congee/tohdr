//! Integration tests for `tohdr_core::apple`.
//!
//! Pure math only: no file I/O. Real-file anchors below are hardcoded
//! constants measured once via
//! `nix run nixpkgs#exiftool -- -MakerNotes:all -HDRGainMapHeadroom <file>`
//! against the (untouched, read-only) files named in each test.

use tohdr_core::apple::{headroom_from_tags, tags_from_headroom};

/// Linear headroom sweep covering the full range `tags_from_headroom` accepts
/// (SDR through 4 stops), including the exact branch-boundary and clamp
/// points where a bug would most likely hide.
fn headroom_sweep() -> Vec<f32> {
    let mut v: Vec<f32> = (0..=800).map(|i| 1.0 + i as f32 * 0.02).collect(); // 1.0..=17.0
    v.extend([1.0, 4.880772, 8.0, 8.0000001, 11.863581, 16.0]);
    v
}

#[test]
fn round_trips_within_exact_representable_range() {
    // Below 8x (3.0 stops) Apple's formula is exactly representable, so the
    // round trip should be tight.
    for i in 0..=300 {
        let headroom = 1.0 + i as f32 * 0.02; // 1.0..=7.0
        let (tag33, tag48) = tags_from_headroom(headroom);
        let back = headroom_from_tags(tag33, tag48);
        assert!(
            (back - headroom).abs() < 0.01,
            "headroom={headroom} -> tags=({tag33},{tag48}) -> back={back}"
        );
    }
}

#[test]
fn clamps_above_8x_instead_of_reproducing_the_washout_bug() {
    // Above 8x (3.0 stops) the formula clamps (documented, lossy): tag48
    // pins at 0.0 and headroom_from_tags reads back exactly 8.0. This is the
    // behavior that must never regress to a negative tag48.
    for headroom in [8.5_f32, 10.0, 11.863581, 14.0, 16.0] {
        let (tag33, tag48) = tags_from_headroom(headroom);
        assert_eq!(tag33, 1.0);
        assert_eq!(tag48, 0.0, "headroom={headroom} should clamp tag48 to 0.0");
        let back = headroom_from_tags(tag33, tag48);
        assert!(
            (back - 8.0).abs() < 0.01,
            "clamped round trip should read back ~8.0, got {back}"
        );
    }
}

#[test]
fn tags_from_headroom_never_produces_a_negative_tag48() {
    // The actual bug being prevented: chemharuka/toGainMapHDR's inverse goes
    // negative above 3.0 stops. Hard invariant across the whole accepted
    // range, order-independent.
    for &headroom in &headroom_sweep() {
        let (tag33, tag48) = tags_from_headroom(headroom);
        assert!(
            tag48 >= 0.0,
            "tag48 went negative at headroom={headroom}: tag48={tag48}"
        );
        assert_eq!(tag33, 1.0);
    }
}

#[test]
fn tags_from_headroom_is_monotonic_non_increasing() {
    // tag48 must never increase as headroom increases -- requires a sorted
    // sweep (headroom_sweep() appends anchors out of order).
    let mut sweep = headroom_sweep();
    sweep.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mut prev_tag48 = f64::INFINITY;
    for &headroom in &sweep {
        let (_, tag48) = tags_from_headroom(headroom);
        assert!(
            tag48 <= prev_tag48 + 1e-9,
            "non-monotonic at headroom={headroom}: prev={prev_tag48} now={tag48}"
        );
        prev_tag48 = tag48;
    }
}

// --- Real-file anchors, via `exiftool -MakerNotes:all -HDRGainMapHeadroom` ---
//
//   IMG_4913.HEIC   (iPhone, correct)  tag33 1.00999999  tag48  0.05253907666  headroom  4.880772
//   DSC07752.heic   (washed out)       tag33 1           tag48 -0.008120966145 headroom 11.863581

#[test]
fn reproduces_iphone_reference_headroom() {
    let headroom = headroom_from_tags(1.00999999, 0.05253907666);
    assert!(
        (headroom - 4.880772).abs() < 0.01,
        "expected ~4.880772, got {headroom}"
    );
}

#[test]
fn reproduces_broken_export_headroom_from_its_negative_tag48() {
    // headroom_from_tags must faithfully decode the broken file's already
    // negative tag48 (that's the historical record) -- the fix belongs in
    // tags_from_headroom (the encoder), not here.
    let headroom = headroom_from_tags(1.0, -0.008120966145);
    assert!(
        (headroom - 11.863581).abs() < 0.01,
        "expected ~11.863581, got {headroom}"
    );
}

#[test]
fn correct_encoder_would_not_have_produced_a_negative_tag48_for_the_broken_file() {
    // If the tool that produced DSC07752.heic had used tags_from_headroom
    // instead of its own broken inverse, its 11.86x headroom (above the
    // 8x/3.0-stop representable ceiling) would have clamped to a non-negative
    // tag48, not gone to -0.008.
    let (tag33, tag48) = tags_from_headroom(11.863581);
    assert_eq!(tag33, 1.0);
    assert!(tag48 >= 0.0);
}
