//! Extended-range HDR pipeline: tone mapping, headroom-consistent derivation,
//! and the weight function that makes an over-declared headroom visible.

use tohdr_core::derive::DeriveOptions;
use tohdr_core::hdr::{apply_hdr, derive_consistent, gain_weight, HdrRgb, ToneMap};
use tohdr_core::{GainMapMeta, GainPlane};

/// A gradient that runs well above SDR white, so the gain map has real work to
/// do. `peak` is the brightest linear luma present.
fn hdr_ramp(w: u32, h: u32, peak: f32) -> HdrRgb {
    let mut img = HdrRgb::black(w, h);
    for y in 0..h {
        for x in 0..w {
            let t = (x as f32 + 0.5) / w as f32;
            // Slight per-channel tilt so chroma preservation is exercised too.
            let v = t * peak;
            let i = (y as usize * w as usize + x as usize) * 3;
            img.data[i] = v;
            img.data[i + 1] = v * 0.9;
            img.data[i + 2] = v * 0.8;
        }
    }
    img
}

#[test]
fn reinhard_maps_white_to_one_and_is_monotonic() {
    let peak = 8.0;
    let tm = ToneMap::Reinhard { white: peak };
    let hdr = hdr_ramp(64, 4, peak);
    let sdr = tm.to_sdr(&hdr);
    assert_eq!(sdr.bits, 8);
    assert_eq!(sdr.data.len(), sdr.expected_len());

    // Brightest column is the HDR peak; Reinhard maps white -> 1.0, so the red
    // channel (the largest) must reach 8-bit full scale.
    let last = ((4 / 2) * 64 + 63) * 3;
    assert_eq!(sdr.data[last], 255, "peak should map to SDR white");

    // Monotonic across the ramp.
    let row = 2usize;
    let mut prev = 0u16;
    for x in 0..64usize {
        let v = sdr.data[(row * 64 + x) * 3];
        assert!(v >= prev, "non-monotonic at x={x}: {prev} -> {v}");
        prev = v;
    }
}

#[test]
fn clip_discards_above_white_but_reinhard_does_not() {
    let hdr = hdr_ramp(32, 2, 6.0);
    let clipped = ToneMap::Clip.to_sdr(&hdr);
    let rolled = ToneMap::Reinhard { white: 6.0 }.to_sdr(&hdr);

    // Clip pins everything at/above SDR white to 255; Reinhard keeps those
    // pixels distinguishable, which is the whole point of the roll-off.
    let clipped_saturated = clipped.data.iter().step_by(3).filter(|&&v| v == 255).count();
    let rolled_saturated = rolled.data.iter().step_by(3).filter(|&&v| v == 255).count();
    assert!(
        clipped_saturated > rolled_saturated,
        "clip {clipped_saturated} should saturate more than reinhard {rolled_saturated}"
    );
}

#[test]
fn derive_consistent_always_holds_the_invariant() {
    // The invariant IMG_4913 holds and both washed-out exports break.
    for peak in [1.0f32, 2.0, 4.88, 11.86, 50.0] {
        let hdr = hdr_ramp(48, 8, peak);
        let sdr = ToneMap::Reinhard { white: peak }.to_sdr(&hdr);
        let (_plane, meta) = derive_consistent(&hdr, &sdr, &DeriveOptions::default());
        assert_eq!(
            meta.max_log2[0], meta.alt_headroom,
            "peak {peak}: declared headroom must equal what the plane encodes"
        );
        assert!(meta.alt_headroom >= 0.0, "peak {peak}: negative headroom");
    }
}

#[test]
fn hdr_round_trip_recovers_above_white_light() {
    let peak = 6.0;
    let hdr = hdr_ramp(64, 16, peak);
    let sdr = ToneMap::Reinhard { white: peak }.to_sdr(&hdr);
    let opts = DeriveOptions {
        subsample: 1,
        // Apple's real offsets; the 1/64 defaults lift near-black far harder.
        base_offset: 1.0e-5,
        alt_offset: 1.0e-5,
        gamma: 1.0,
        ..DeriveOptions::default()
    };
    let (plane, meta) = derive_consistent(&hdr, &sdr, &opts);
    let back = apply_hdr(&sdr, &plane, &meta);

    // Reconstruction must actually exceed SDR white -- if it clamped at 1.0
    // (the bug `apply_hdr` exists to avoid) this fails outright.
    let recovered_peak = (0..back.data.len()).map(|i| back.data[i]).fold(0.0f32, f32::max);
    assert!(
        recovered_peak > 3.0,
        "reconstruction clamped: peak {recovered_peak}, expected near {peak}"
    );

    // Relative error on luma, skipping the darkest pixels where the 8-bit base
    // quantization dominates the ratio.
    let mut worst = 0.0f32;
    for y in 0..hdr.height {
        for x in 0..hdr.width {
            let want = hdr.luma(x, y);
            if want < 0.05 {
                continue;
            }
            let got = back.luma(x, y);
            worst = worst.max((got - want).abs() / want);
        }
    }
    assert!(worst < 0.06, "worst relative luma error {worst}");
}

#[test]
fn weight_matches_libavif_including_the_negative_branch() {
    let m = |base: f32, alt: f32| GainMapMeta {
        base_headroom: base,
        alt_headroom: alt,
        ..GainMapMeta::default()
    };

    // Equal headrooms: libavif declines to apply the map (`src/gainmap.c:56`).
    assert_eq!(gain_weight(&m(2.0, 2.0), 5.0), 0.0);

    // Normal case: linear in stops, clamped at both ends.
    assert_eq!(gain_weight(&m(0.0, 4.0), 0.0), 0.0);
    assert_eq!(gain_weight(&m(0.0, 4.0), 2.0), 0.5);
    assert_eq!(gain_weight(&m(0.0, 4.0), 4.0), 1.0);
    assert_eq!(gain_weight(&m(0.0, 4.0), 9.0), 1.0, "must clamp, not extrapolate");

    // alt < base, display below base: both terms negative -> positive ratio,
    // clamped, then negated. This is the only path that returns < 0.
    assert_eq!(gain_weight(&m(4.0, 0.0), 2.0), -0.5);
    assert_eq!(gain_weight(&m(4.0, 0.0), 4.0), -0.0);
    // alt < base, display above base: negative ratio clamps to 0 before the flip.
    assert_eq!(gain_weight(&m(4.0, 0.0), 6.0), 0.0);
}

#[test]
fn over_declared_headroom_under_applies_the_map() {
    // DSC07752_iso.heic, decoded from assets/fixtures/dsc07752_iso21496.bin:
    // it declares 3.568470 stops while its plane only encodes 1.96.
    let broken = GainMapMeta {
        max_log2: [1.96; 3],
        base_headroom: 0.0,
        alt_headroom: 3.568470,
        ..GainMapMeta::default()
    };
    // IMG_4913.HEIC keeps the two equal.
    let good = GainMapMeta {
        max_log2: [2.287109; 3],
        base_headroom: 0.0,
        alt_headroom: 2.287109,
        ..GainMapMeta::default()
    };

    // A phone at ~2.3 stops.
    let phone = 2.3;
    let w_broken = gain_weight(&broken, phone);
    let w_good = gain_weight(&good, phone);
    assert!((w_broken - 0.6445).abs() < 1e-3, "phone weight {w_broken}");
    assert_eq!(w_good, 1.0, "the good file's map applies fully");

    // Delivered stops: the broken file hands the phone well under its own plane.
    let delivered = broken.max_log2[0] * w_broken;
    assert!(
        delivered < 1.3,
        "expected under 1.3 of 1.96 stops, got {delivered}"
    );

    // A Mac XDR at ~2.98 stops gets noticeably more of the same file, which is
    // the reported Mac-fine / phone-washed asymmetry.
    let w_mac = gain_weight(&broken, 2.98);
    assert!(
        w_mac - w_broken > 0.15,
        "expected a clear phone/Mac gap, got {w_broken} vs {w_mac}"
    );
}

#[test]
fn peak_luma_ignores_a_lone_hot_pixel() {
    let mut img = hdr_ramp(64, 64, 4.0);
    // One absurd pixel: 1000x SDR white.
    img.data[0] = 1000.0;
    img.data[1] = 1000.0;
    img.data[2] = 1000.0;

    let trimmed = img.peak_luma(0.001);
    assert!(
        trimmed < 8.0,
        "outlier trim should reject the hot pixel, got {trimmed}"
    );
    // With zero tolerance the outlier is kept, proving the trim is what helped.
    let untrimmed = img.peak_luma(0.0);
    assert!(
        untrimmed > 100.0,
        "zero-tolerance peak should see the hot pixel, got {untrimmed}"
    );
}

#[test]
fn peak_luma_floors_at_sdr_white() {
    // An all-SDR image has no headroom; never report less than 1.0, which would
    // make alt_headroom negative.
    let dim = hdr_ramp(16, 16, 0.25);
    assert_eq!(dim.peak_luma(0.01), 1.0);
}

#[test]
fn sdr_only_source_declares_no_headroom() {
    let hdr = hdr_ramp(32, 4, 1.0);
    let sdr = ToneMap::Clip.to_sdr(&hdr);
    let (plane, meta) = derive_consistent(&hdr, &sdr, &DeriveOptions::default());
    assert_eq!(meta.max_log2[0], meta.alt_headroom);
    assert!(
        meta.alt_headroom < 0.15,
        "an SDR source should declare ~no headroom, got {}",
        meta.alt_headroom
    );
    assert_eq!(plane.data.len(), plane.expected_len());
}

#[test]
fn apply_hdr_handles_a_degenerate_zero_range_plane() {
    let base = ToneMap::Clip.to_sdr(&hdr_ramp(8, 8, 1.0));
    let plane = GainPlane {
        width: 8,
        height: 8,
        data: vec![0; 64],
    };
    let meta = GainMapMeta {
        min_log2: [0.0; 3],
        max_log2: [0.0; 3],
        ..GainMapMeta::default()
    };
    let out = apply_hdr(&base, &plane, &meta);
    assert_eq!(out.width, 8);
    assert!(out.data.iter().all(|v| v.is_finite()), "non-finite output");
}
