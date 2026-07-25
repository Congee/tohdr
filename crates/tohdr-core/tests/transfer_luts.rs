//! The sRGB lookup tables must be indistinguishable from the curves they
//! replace. A table that is merely close is a silent, image-wide colour shift,
//! so these check every representable input rather than sampling.

use tohdr_core::derive::{linear_to_srgb, linear_to_srgb8, srgb8_to_linear, srgb_to_linear};

/// Exhaustive: all 256 codes. The 8-bit decode table is not an approximation,
/// so the tolerance is float-rounding only.
#[test]
fn srgb8_decode_table_is_exact_for_every_code() {
    for code in 0u16..=255 {
        let want = srgb_to_linear(code as f32 / 255.0);
        let got = srgb8_to_linear(code as u8);
        assert!(
            (got - want).abs() < 1e-7,
            "code {code}: table {got:.9}, curve {want:.9}"
        );
    }
}

#[test]
fn srgb8_decode_endpoints_and_monotonicity() {
    assert_eq!(srgb8_to_linear(0), 0.0);
    assert!((srgb8_to_linear(255) - 1.0).abs() < 1e-6);
    let mut prev = -1.0;
    for code in 0u16..=255 {
        let v = srgb8_to_linear(code as u8);
        assert!(v >= prev, "not monotonic at {code}");
        prev = v;
    }
    // Textbook midpoint, independent of our own implementation.
    assert!((srgb8_to_linear(128) - 0.215861).abs() < 1e-4);
}

/// The encode table quantizes its *input*, so the bar is that it never differs
/// from the exact curve by more than one 8-bit code. Swept far more finely
/// than the table itself, to catch interpolation error between entries.
#[test]
fn srgb8_encode_table_never_differs_by_more_than_one_code() {
    let steps = 200_000;
    let mut worst = 0i32;
    let mut worst_at = 0.0f32;
    for i in 0..=steps {
        let lin = i as f32 / steps as f32;
        let exact = (linear_to_srgb(lin) * 255.0).round() as i32;
        let table = linear_to_srgb8(lin) as i32;
        let d = (exact - table).abs();
        if d > worst {
            worst = d;
            worst_at = lin;
        }
    }
    assert!(
        worst <= 1,
        "worst error {worst} codes at linear {worst_at:.6}; the table must \
         stay within one code of the exact curve"
    );
}

#[test]
fn srgb8_encode_endpoints_and_monotonicity() {
    assert_eq!(linear_to_srgb8(0.0), 0);
    assert_eq!(linear_to_srgb8(1.0), 255);
    // Out-of-range input must clamp, not index out of bounds.
    assert_eq!(linear_to_srgb8(-5.0), 0);
    assert_eq!(linear_to_srgb8(17.0), 255);
    assert_eq!(linear_to_srgb8(f32::NAN), 0, "NaN must not panic or wrap");

    let mut prev = 0u8;
    for i in 0..=10_000 {
        let v = linear_to_srgb8(i as f32 / 10_000.0);
        assert!(v >= prev, "encode not monotonic at {i}");
        prev = v;
    }
}

/// Decode-then-encode must return the original code for all 256 of them.
/// This is the round trip the tone-map and derive paths actually perform.
#[test]
fn round_trip_through_both_tables_is_the_identity() {
    for code in 0u16..=255 {
        let lin = srgb8_to_linear(code as u8);
        let back = linear_to_srgb8(lin);
        assert_eq!(
            back, code as u8,
            "code {code} -> linear {lin:.9} -> code {back}"
        );
    }
}
