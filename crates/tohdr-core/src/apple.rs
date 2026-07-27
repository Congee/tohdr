//! Apple MakerNote HDR headroom tags.
//!
//! Tag 33 (`HDRHeadroom`) and tag 48 (`HDRGain`) together encode the display
//! headroom Apple's renderers use. Getting these wrong is what makes an
//! otherwise-valid gain-map HEIC render washed out in Apple-stack consumers.
//!
//! **Units: public boundaries are linear headroom, every internal `stops` is
//! log2.** The tag33/tag48 formula is piecewise-linear in stops. Confusing the
//! two is the bug that washes out other tools' output.

/// Decode MakerApple tag33/tag48 to linear HDR headroom.
///
/// Ported from Skia's `get_maker_note_hdr_headroom` (`src/codec/SkExif.cpp`,
/// BSD-3-Clause): tag33 selects a coefficient set at the 1.0 threshold, tag48 a
/// sub-branch at 0.01, then clamp to non-negative stops and exponentiate.
/// Coefficients cross-checked against HDR2gainmapApp (MIT), which agrees but
/// forgets to exponentiate.
pub fn headroom_from_tags(tag33: f64, tag48: f64) -> f32 {
    // SkExif.cpp:83-95.
    let stops = if tag33 < 1.0 {
        if tag48 <= 0.01 {
            -20.0 * tag48 + 1.8
        } else {
            -0.101 * tag48 + 1.601
        }
    } else if tag48 <= 0.01 {
        -70.0 * tag48 + 3.0
    } else {
        -0.303 * tag48 + 2.303
    };
    // Upper bound Skia does not need and we do: these tags come from an
    // untrusted MakerNote, and the formula has no domain check, so a corrupt
    // tag48 of -1e30 gives 2^7e31 = +inf headroom flowing out to every consumer.
    // 16 stops is 65536x, past any display.
    const MAX_STOPS: f64 = 16.0;
    let stops = if stops.is_nan() { 0.0 } else { stops.clamp(0.0, MAX_STOPS) };
    2f64.powf(stops) as f32
}

/// Where encoding switches between the two sub-branches of the tag33>=1.0
/// regime. Solved so they meet exactly: Apple's nominal boundary (stops == 2.3)
/// leaves a ~1e-4 discontinuity, and encoding across that gap would violate
/// monotonicity.
const STOPS_BRANCH_BOUNDARY: f64 = (70.0 * 2.303 - 0.303 * 3.0) / (70.0 - 0.303);

/// Apple's tag33>=1.0 formula (see [`headroom_from_tags`]) cannot express
/// more than this many stops without tag48 going negative: at 3.0 stops the
/// steep branch `(3.0 - stops) / 70.0` already reaches its floor of 0.0.
const MAX_REPRESENTABLE_STOPS: f64 = 3.0;

/// Encode linear HDR headroom to MakerApple `(tag33, tag48)`.
///
/// Apple documents no inverse, and the obvious one is a trap: emitting
/// `tag48 = (3.0 - stops) / 70.0` unclamped makes tag48 *negative* above 8x, and
/// Apple-stack decoders then read back less headroom than 8x -- a washed-out
/// render. The Apple-flavor export (headroom 11.86, tag48 -0.008) is exactly that bug.
///
/// So: always `tag33 = 1.0` (Skia only tests the threshold, never the magnitude),
/// branches inverted at [`STOPS_BRANCH_BOUNDARY`], and a hard clamp at
/// [`MAX_REPRESENTABLE_STOPS`] rather than extrapolation. The clamp is lossy --
/// above 8x a round trip reads back exactly 8x -- but it is monotonic and never
/// negative, which is the property that matters.
///
/// `headroom` is clamped into `1.0..=16.0` first.
pub fn tags_from_headroom(headroom: f32) -> (f64, f64) {
    let stops = (headroom.clamp(1.0, 16.0) as f64).log2();
    let tag48 = if stops >= MAX_REPRESENTABLE_STOPS {
        0.0
    } else if stops >= STOPS_BRANCH_BOUNDARY {
        (3.0 - stops) / 70.0
    } else {
        (2.303 - stops) / 0.303
    };
    (1.0, tag48.max(0.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn continuous_at_branch_boundary() {
        // Both branch formulas evaluated at the exact crossover must agree
        // (that's what STOPS_BRANCH_BOUNDARY is solved for) -- this is what
        // keeps tags_from_headroom monotonic across the branch switch.
        let shallow = (2.303 - STOPS_BRANCH_BOUNDARY) / 0.303;
        let steep = (3.0 - STOPS_BRANCH_BOUNDARY) / 70.0;
        assert!((shallow - steep).abs() < 1e-9, "shallow={shallow} steep={steep}");
    }
}
