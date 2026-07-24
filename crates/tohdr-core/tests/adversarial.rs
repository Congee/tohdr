//! Regressions found by adversarial review.
//!
//! Each test here failed when it was written; the fix followed.

use tohdr_core::derive::{derive, DeriveOptions};
use tohdr_core::hdr::{derive_consistent, gain_weight, HdrRgb, ToneMap};
use tohdr_core::{GainMapMeta, Rgb};

fn hdr_from(width: u32, height: u32, px: &[[f32; 3]]) -> HdrRgb {
    let mut data = Vec::with_capacity((width * height) as usize * 3);
    for i in 0..(width * height) as usize {
        data.extend_from_slice(&px[i % px.len()]);
    }
    HdrRgb { width, height, data }
}

/// One `+inf` sample used to zero the entire gain map.
///
/// The non-finite guard reset *both* bounds to 0, so `range == 0`, every
/// normalized sample became 0, and a frame with genuine highlights everywhere
/// else reconstructed as the flat SDR base. A single defective pixel — an EXR
/// artifact, an exposure-fusion overflow — destroyed the whole image's HDR.
#[test]
fn one_infinite_sample_does_not_wipe_the_whole_gain_map() {
    // Half the pixels carry a real ~1-stop highlight; one carries +inf.
    let w = 8;
    let h = 4;
    let mut data = Vec::new();
    for i in 0..(w * h) as usize {
        if i == 5 {
            data.extend_from_slice(&[f32::INFINITY, f32::INFINITY, f32::INFINITY]);
        } else if i % 2 == 0 {
            data.extend_from_slice(&[2.0, 2.0, 2.0]);
        } else {
            data.extend_from_slice(&[1.0, 1.0, 1.0]);
        }
    }
    let hdr = HdrRgb { width: w, height: h, data };
    let base = ToneMap::Clip.to_sdr(&hdr);

    let (plane, meta) = derive_consistent(&hdr, &base, &DeriveOptions::default());

    assert!(
        meta.max_log2[0].is_finite(),
        "max_log2 must stay finite, got {}",
        meta.max_log2[0]
    );
    assert!(
        meta.max_log2[0] > 0.1,
        "the legitimate ~1-stop highlights must survive one bad pixel; \
         got max_log2 = {}",
        meta.max_log2[0]
    );
    assert!(
        plane.data.iter().any(|&v| v > 0),
        "an all-zero plane means every pixel's gain was discarded"
    );
}

/// A NaN sample must be equally survivable.
#[test]
fn one_nan_sample_does_not_wipe_the_whole_gain_map() {
    let w = 8;
    let h = 4;
    let mut data = Vec::new();
    for i in 0..(w * h) as usize {
        if i == 3 {
            data.extend_from_slice(&[f32::NAN, f32::NAN, f32::NAN]);
        } else if i % 2 == 0 {
            data.extend_from_slice(&[2.0, 2.0, 2.0]);
        } else {
            data.extend_from_slice(&[1.0, 1.0, 1.0]);
        }
    }
    let hdr = HdrRgb { width: w, height: h, data };
    let base = ToneMap::Clip.to_sdr(&hdr);
    let (_, meta) = derive_consistent(&hdr, &base, &DeriveOptions::default());
    assert!(meta.max_log2[0].is_finite());
    assert!(meta.min_log2[0].is_finite());
    assert!(
        meta.max_log2[0] > 0.1,
        "NaN pixel wiped the map: max_log2 = {}",
        meta.max_log2[0]
    );
}

/// `derive_consistent` exists to guarantee `max_log2 == alt_headroom`. It
/// clamped only `alt_headroom` to non-negative, so a base brighter than the
/// source left `max_log2` negative and the two disagreeing — the very
/// invariant violation the function is named for.
///
/// Worse, with `base_headroom == 0` the result was `base == alt == 0`, which
/// makes `gain_weight` return 0 for *every* display: the map is switched off
/// entirely rather than merely being small.
#[test]
fn invariant_holds_even_when_the_base_is_brighter_than_the_source() {
    // An independently graded SDR base with lifted shadows, brighter than the
    // HDR source everywhere: nothing requires the base to be our own tone map.
    let hdr = hdr_from(4, 4, &[[0.5, 0.5, 0.5]]);
    let bright = (0.6f32.powf(1.0 / 2.4) * 1.055 - 0.055).clamp(0.0, 1.0);
    let code = (bright * 255.0).round() as u16;
    let base = Rgb {
        width: 4,
        height: 4,
        bits: 8,
        data: vec![code; 4 * 4 * 3],
    };

    let (_, meta) = derive_consistent(&hdr, &base, &DeriveOptions::default());

    assert!(
        (meta.max_log2[0] - meta.alt_headroom).abs() < 1e-6,
        "the whole point of derive_consistent: max_log2 ({}) must equal \
         alt_headroom ({})",
        meta.max_log2[0],
        meta.alt_headroom
    );
    assert!(
        meta.base_headroom != meta.alt_headroom,
        "base == alt makes gain_weight return 0 for every display, silently \
         disabling the map (base={}, alt={})",
        meta.base_headroom,
        meta.alt_headroom
    );
}

/// `headroom_from_tags` reads untrusted MakerNote values. A wildly negative
/// tag48 drove `2^stops` past f64's range and returned `+inf` as a "linear
/// headroom", which then flowed into `GainMapMeta`.
#[test]
fn corrupt_maker_note_tags_cannot_produce_infinite_headroom() {
    for (t33, t48) in [
        (1.0, -1000.0),
        (1.0, -1e30),
        (0.0, -1e30),
        (f64::NAN, 0.0),
        (1.0, f64::NEG_INFINITY),
    ] {
        let h = tohdr_core::apple::headroom_from_tags(t33, t48);
        assert!(
            h.is_finite(),
            "tag33={t33} tag48={t48} produced non-finite headroom {h}"
        );
        assert!(h >= 1.0, "headroom below unity is meaningless: {h}");
    }
}

/// A crafted ISO payload can encode `max_log2` near 32768; `apply_hdr` then
/// computes `exp2` of it and every sample becomes infinite. Parsing must
/// reject values no real capture can produce rather than passing them on.
#[test]
fn iso_parse_rejects_absurd_log2_ranges() {
    let mut meta = GainMapMeta::default();
    meta.max_log2 = [30000.0; 3];
    meta.alt_headroom = 30000.0;
    let bytes = tohdr_core::iso21496::serialize(&meta);
    let parsed = tohdr_core::iso21496::parse(&bytes);
    assert!(
        parsed.is_err(),
        "a 30000-stop headroom must be rejected, got {:?}",
        parsed.map(|m| m.max_log2[0])
    );
}

/// Sanity: the ordinary path still works after the hardening above.
#[test]
fn a_normal_derive_is_unaffected() {
    let hdr = hdr_from(16, 16, &[[4.0, 4.0, 4.0], [1.0, 1.0, 1.0], [0.25, 0.25, 0.25]]);
    let base = ToneMap::Reinhard { white: hdr.peak_luma(0.001) }.to_sdr(&hdr);
    let (plane, meta) = derive_consistent(&hdr, &base, &DeriveOptions::default());
    assert!((meta.max_log2[0] - meta.alt_headroom).abs() < 1e-6);
    assert!(meta.max_log2[0] > 0.0);
    assert!(plane.data.iter().any(|&v| v > 0));
    assert_eq!(gain_weight(&meta, meta.alt_headroom), 1.0);
}

/// The fixed-range `derive` entry point shares the kernel, so it inherits the
/// same non-finite handling.
#[test]
fn fixed_range_derive_still_agrees_with_itself() {
    let px = vec![200u16; 8 * 8 * 3];
    let hdr = Rgb { width: 8, height: 8, bits: 8, data: px.clone() };
    let base = Rgb { width: 8, height: 8, bits: 8, data: px };
    let (_, meta) = derive(&hdr, &base, &DeriveOptions::default());
    assert!(meta.min_log2[0].is_finite() && meta.max_log2[0].is_finite());
}
