//! Apple MakerNote HDR headroom tags.
//!
//! Tag 33 (`HDRHeadroom`) and tag 48 (`HDRGain`) together encode the display
//! headroom Apple's renderers use. Getting these wrong is what makes an
//! otherwise-valid gain-map HEIC render washed out in Apple-stack consumers.
//!
//! ## Units
//!
//! Both functions in this module speak **linear headroom** at their public
//! boundary (`4.0` means "4x brighter than SDR white", matching the
//! `HDRGainMapHeadroom` XMP/exif tag). Internally the tag33/tag48 formula is
//! piecewise-linear in **log2 stops** (`log2(4.0) == 2.0` stops); every
//! internal `stops` variable in this file is log2, every public `headroom`
//! parameter/return is linear. This distinction is exactly what the
//! anti-reference tool (see [`tags_from_headroom`] docs) got wrong.

/// Decode MakerApple tag33/tag48 to linear HDR headroom.
///
/// Ported from Skia's `get_maker_note_hdr_headroom`
/// (`src/codec/SkExif.cpp:82-96`, google/skia@main, BSD-3-Clause): piecewise
/// linear in log2 stops, tag33 selects a coefficient set at the 1.0
/// threshold, tag48 picks a sub-branch at the 0.01 threshold, then the result
/// is clamped to non-negative stops and exponentiated
/// (`SkExif.cpp:96`, `std::pow(2.f, std::max(stops, 0.f))`).
///
/// Independently confirmed against HDR2gainmapApp's
/// `readMakerAppleHeadroom` (`Scripts/fix_gainmap.swift:284-291`,
/// vastunghia/HDR2gainmapApp, MIT): identical coefficients and branch
/// structure. That script, however, returns the raw `stops` value as if it
/// *were* the linear headroom — it never exponentiates
/// (`fix_gainmap.swift:286`, `288`) — so on its own it does not reproduce
/// `HDRGainMapHeadroom`; see [`tags_from_headroom`] docs for the numeric
/// check that caught this.
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
    // SkExif.cpp:96.
    2f64.powf(stops.max(0.0)) as f32
}

/// The log2-stops point at which encoding switches from the shallow
/// (tag48 > 0.01) to the steep (tag48 <= 0.01) branch of the tag33>=1.0
/// regime. Solved so the two branches meet exactly (see module tests): naively
/// using Apple/Skia's own nominal boundary (stops == 2.3, i.e. tag48 == 0.01)
/// leaves a ~1e-4 discontinuity because `2.303` and `0.303`/`70.0` don't
/// divide evenly — encoding across that gap would very slightly violate
/// monotonicity. Solving `(3.0 - s) / 70.0 == (2.303 - s) / 0.303` for `s`
/// removes the gap without changing either coefficient.
const STOPS_BRANCH_BOUNDARY: f64 = (70.0 * 2.303 - 0.303 * 3.0) / (70.0 - 0.303);

/// Apple's tag33>=1.0 formula (see [`headroom_from_tags`]) cannot express
/// more than this many stops without tag48 going negative: at 3.0 stops the
/// steep branch `(3.0 - stops) / 70.0` already reaches its floor of 0.0.
const MAX_REPRESENTABLE_STOPS: f64 = 3.0;

/// Encode linear HDR headroom to MakerApple `(tag33, tag48)`.
///
/// This is the direction Apple's own spec never documents an inverse for —
/// [`headroom_from_tags`] decodes tags written by Apple's own encoder, but
/// nothing here says how to *produce* tags for an arbitrary headroom. That
/// gap is what chemharuka/toGainMapHDR's `main.swift` fell into: for
/// `stops >= 2.3` it emits `tag48 = (3.0 - stops) / 70.0` with no upper
/// clamp, so any headroom above 8x (3.0 stops) makes `tag48` negative. The
/// broken export analyzed here (`DSC07752.heic`, `HDRGainMapHeadroom` =
/// 11.86, i.e. `log2(11.86) ≈ 3.567` stops) is squarely in that regime:
/// `(3.0 - 3.567) / 70.0 ≈ -0.0081`, matching its measured tag48 of `-0.008`
/// almost exactly. That negative tag48 is what Apple-stack decoders
/// (including this module's own [`headroom_from_tags`], and Skia's) read
/// back as *less* headroom than 8x, i.e. a washed-out render.
///
/// This function always emits `tag33 = 1.0` (the ">=1.0" branch; Skia's
/// parser only tests the 1.0 threshold, never tag33's exact magnitude, so
/// 1.0 vs the iPhone reference's 1.01 decode identically) and inverts the two
/// sub-branches of that regime at [`STOPS_BRANCH_BOUNDARY`] instead of
/// Apple's own (slightly discontinuous) nominal boundary.
///
/// Apple's formula has no representation above 3.0 stops (8x linear
/// headroom) without going negative (see [`MAX_REPRESENTABLE_STOPS`]); this
/// function clamps explicitly there instead of extrapolating: for any
/// headroom above 8x, up through the 16x (4.0-stop) ceiling this function
/// accepts, `tag48` saturates at `0.0`. That clamp is lossy — a round trip
/// through [`headroom_from_tags`] reads back exactly 8x, not the true
/// headroom — but it is monotonic non-increasing and never negative, which
/// is the actual property that matters: no output of this function can ever
/// reproduce the washout bug above.
///
/// `headroom` below 1.0 (i.e. no headroom, an SDR image) is clamped up to
/// 1.0 before conversion; headroom above 16.0 is clamped down to 16.0.
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
