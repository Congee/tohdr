//! `tohdr verify`: does a file hold the correctness invariants that separate
//! `IMG_4913.HEIC` from the washed-out exports in
//! `docs/heic-gainmap-structure.md`? Exits non-zero on any failed check, so
//! it is usable as a CI gate and from the Lightroom plugin.

use std::path::PathBuf;

use anyhow::Context;
use serde::Serialize;
use tohdr_apple::ReadBack;
use tohdr_core::hdr::gain_weight;

use crate::cli::VerifyArgs;
use crate::panic_guard::catch;

/// Bundled reference file used when `--against` is omitted: the iPhone
/// capture that renders correctly everywhere tested.
/// Reference file `--against` falls back to when the caller names none.
///
/// Overridable via `TOHDR_REFERENCE`, because the default is one particular
/// iPhone capture that exists only on the machine this was developed on. Its
/// absence is not an error: the reference is printed beside the target for
/// human comparison and never folded into the pass/fail decision.
fn default_reference() -> Option<std::path::PathBuf> {
    if let Ok(p) = std::env::var("TOHDR_REFERENCE") {
        if !p.is_empty() {
            return Some(std::path::PathBuf::from(p));
        }
    }
    let p = std::path::PathBuf::from("~/Downloads/IMG_4913.HEIC");
    p.exists().then_some(p)
}

/// A phone-class display headroom, for [`gain_weight`] reporting.
const PHONE_HEADROOM_STOPS: f32 = 2.3;
/// A Mac Studio Display XDR-class display headroom.
const MAC_XDR_HEADROOM_STOPS: f32 = 2.98;

#[derive(Serialize, Debug, Clone)]
pub struct Check {
    pub name: String,
    pub passed: bool,
    pub detail: String,
}

#[derive(Serialize, Debug)]
struct VerifyReport {
    file: String,
    reference: Option<String>,
    checks: Vec<Check>,
    passed: bool,
}

/// Run every check against one file's [`ReadBack`]. Pure and unit-testable:
/// no I/O, no ImageIO call.
pub fn checks_for(rb: &ReadBack) -> Vec<Check> {
    let mut checks = Vec::new();

    checks.push(Check {
        name: "flavor_presence".into(),
        passed: rb.apple_aux || rb.iso_aux,
        detail: format!("apple={} iso={}", rb.apple_aux, rb.iso_aux),
    });

    // The format must actually *be* `L008`, not merely be reported. Checking
    // only `is_some()` passed `DSC07752_iso.heic` — whose plane is `420f`,
    // 3-channel 4:2:0 float, the exact defect criterion 2 names for that file
    // in `docs/acceptance-criteria.md`'s comparison table. It escaped notice
    // because criteria 5 and 8 also fail on that file, so the exit code was
    // right for unrelated reasons; a regression that broke only the channel
    // count would have shipped a clean PASS.
    const WANT_FMT: u32 = u32::from_be_bytes(*b"L008");
    checks.push(Check {
        name: "gain_plane_present".into(),
        passed: rb.gain_size.is_some() && rb.gain_pixel_format == Some(WANT_FMT),
        detail: match (rb.gain_size, rb.gain_pixel_format) {
            (Some((w, h)), Some(fmt)) => format!(
                "{w}x{h}, format {}{}",
                String::from_utf8_lossy(&fmt.to_be_bytes()),
                if fmt == WANT_FMT {
                    ""
                } else {
                    " -- want L008: single-channel 8-bit, not a multi-channel plane"
                }
            ),
            (Some((w, h)), None) => format!("{w}x{h} but no pixel format reported"),
            _ => "no gain-map plane found".into(),
        },
    });

    match rb.headroom_consistent() {
        None => checks.push(Check {
            name: "headroom_consistent".into(),
            passed: true,
            detail: "no ISO 21496-1 metadata to check (Apple-only file)".into(),
        }),
        Some(consistent) => {
            let m = rb.iso_meta.as_ref().expect("Some(_) implies iso_meta");
            checks.push(Check {
                name: "headroom_consistent".into(),
                passed: consistent,
                detail: format!(
                    "max_log2={:.6} alt_headroom={:.6}{}",
                    m.max_log2[0],
                    m.alt_headroom,
                    if consistent { "" } else { " -- MISMATCH: renderer will under/over-apply the map" }
                ),
            });
        }
    }

    // Absent tags are a *skip*, not a failure: writing none is the one option
    // that never lies, and it is what we do both when the source had no MakerNote
    // and when the headroom exceeds the 3.0 stops Apple can express
    // (docs/acceptance-criteria.md 8). Failing here made every TIFF and JPEG
    // conversion exit non-zero for doing the right thing.
    //
    // It cannot say which of the two happened, because `read::maker_apple_tags`
    // returns `(None, None)` for both an absent MakerApple dict and one without
    // keys 33/48. The bytes do distinguish them -- above the ceiling only the pair
    // is removed, ~23 tags survive -- so with dict-presence in `ReadBack` this arm
    // could fail the one real defect: note present, pair gone, headroom under 3.0.
    //
    // Skip is modelled as pass-with-reason rather than a third `Check` state.
    let (tags_ok, tags_detail) = match (rb.tag33, rb.tag48) {
        (Some(t33), Some(t48)) => (
            t33 >= 0.0 && t48 >= 0.0,
            format!(
                "tag33={t33:.6} tag48={t48:.6}{}",
                if t48 < 0.0 { " -- negative tag48 is the toGainMapHDR bug" } else { "" }
            ),
        ),
        // tag33 picks which regime of Apple's headroom formula applies, so
        // tag48 without it decodes to nothing. Criterion 8 requires the pair;
        // the catch-all arm this replaces judged the case on `apple_aux` and
        // so could pass a file carrying an undecodable tag48.
        (None, Some(t48)) => (
            false,
            format!("tag48={t48:.6} but tag33 is missing -- neither decodes without the other"),
        ),
        (Some(t33), None) => (t33 >= 0.0, format!("tag33={t33:.6}, no tag48 to check")),
        (None, None) => (true, "skipped: no MakerApple headroom tags present".into()),
    };
    checks.push(Check {
        name: "maker_apple_tags_non_negative".into(),
        passed: tags_ok,
        detail: tags_detail,
    });

    // Criterion 6. Absent from this checker until now, so a file declaring a
    // non-zero base headroom — a mis-declared HDR base, or a future writer bug —
    // passed every Rust check while `verify_gainmap.py` failed it twice over.
    if let Some(m) = &rb.iso_meta {
        checks.push(Check {
            name: "base_headroom_zero".into(),
            passed: m.base_headroom.abs() < 1e-6,
            detail: format!(
                "base_headroom={:.6}{}",
                m.base_headroom,
                if m.base_headroom.abs() < 1e-6 {
                    ""
                } else {
                    " -- an SDR base declares zero; non-zero shifts where the map starts applying"
                }
            ),
        });
    }

    // Criterion 10, the real invariant: every display receives all the gain it
    // can show, `delivered == min(display, max_log2)`.
    //
    // This replaces a check that asserted `gain_weight`'s return was within
    // `[-1, 1]`. That was a tautology — `gain_weight` clamps to `[0, 1]` before
    // returning (`tohdr_core::hdr`), then optionally negates — so it could only
    // ever fail on NaN, and it stayed green on a synthetic file whose real
    // delivered gain was off by a full stop. `verify_gainmap.py` has checked the
    // genuine invariant all along; this brings the two into line.
    //
    // `max_log2` is floored at zero for the same reason criterion 5 floors it:
    // the headroom field is unsigned, so a darkening map can deliver no gain and
    // must want none.
    if let Some(m) = &rb.iso_meta {
        let deliverable = m.max_log2[0].max(0.0);
        let mut worst: Option<(f32, f32, f32, f32)> = None;
        for display in [1.0f32, 1.5, 2.0, PHONE_HEADROOM_STOPS, MAC_XDR_HEADROOM_STOPS, 4.0] {
            let delivered = deliverable * gain_weight(m, display);
            let want = display.min(deliverable);
            let err = (delivered - want).abs();
            if worst.is_none_or(|(_, _, _, w)| err > w) {
                worst = Some((display, delivered, want, err));
            }
        }
        let (d, got, want, err) = worst.expect("the sweep is non-empty");
        checks.push(Check {
            name: "every_display_gets_its_stops".into(),
            passed: err < 1e-3,
            detail: format!(
                "worst at {d:.2}-stop display: delivered {got:.3}, expected \
                 min(display, max_log2)={want:.3} (err {err:.3}); \
                 phone weight={:.4}, mac xdr weight={:.4}",
                gain_weight(m, PHONE_HEADROOM_STOPS),
                gain_weight(m, MAC_XDR_HEADROOM_STOPS)
            ),
        });
    }

    checks
}

fn print_human(label: &str, checks: &[Check]) {
    println!("{label}:");
    for c in checks {
        println!("  [{}] {}: {}", if c.passed { "ok" } else { "FAIL" }, c.name, c.detail);
    }
}

/// Criterion 11: no display in 1.0..=4.0 stops receives *less* gain from this
/// file than from the reference, at the same declared headroom.
///
/// "At the same declared headroom" is a condition, not decoration: comparing
/// delivered gain across different declarations measures the two *scenes* rather
/// than the two encoders, so this skips unless the declarations match. What
/// survives is worth catching -- identical metadata that we under-apply.
///
/// Pure, so it is testable without ImageIO; [`run`] folds it into the verdict.
pub fn reference_comparison(rb: &ReadBack, reference: &ReadBack) -> Check {
    let name = "no_worse_than_reference".into();
    let (Some(m), Some(r)) = (&rb.iso_meta, &reference.iso_meta) else {
        return Check {
            name,
            passed: true,
            detail: "skipped: one of the two files has no ISO metadata to compare".into(),
        };
    };
    if (m.alt_headroom - r.alt_headroom).abs() >= 1e-3 {
        return Check {
            name,
            passed: true,
            detail: format!(
                "skipped: declared headroom differs ({:.6} vs reference {:.6}) -- \
                 that compares the scenes, not the encoders",
                m.alt_headroom, r.alt_headroom
            ),
        };
    }
    let ours = m.max_log2[0].max(0.0);
    let theirs = r.max_log2[0].max(0.0);
    let mut worst: Option<(f32, f32, f32)> = None;
    for display in [1.0f32, 1.5, 2.0, PHONE_HEADROOM_STOPS, MAC_XDR_HEADROOM_STOPS, 4.0] {
        let got = ours * gain_weight(m, display);
        let want = theirs * gain_weight(r, display);
        let deficit = want - got;
        if worst.is_none_or(|(_, _, w)| deficit > w) {
            worst = Some((display, got, deficit));
        }
    }
    let (d, got, deficit) = worst.expect("the sweep is non-empty");
    Check {
        name,
        passed: deficit < 1e-3,
        detail: format!(
            "worst at {d:.2}-stop display: delivered {got:.3} vs reference \
             {:.3} (deficit {deficit:.3})",
            got + deficit
        ),
    }
}

pub fn run(args: VerifyArgs) -> anyhow::Result<i32> {
    let path = args.file.as_path();
    eprintln!("tohdr: verifying {} via apple-imageio", path.display());
    let rb = catch("apple-imageio", "inspect", || tohdr_apple::inspect(path))
        .with_context(|| format!("inspecting {}", path.display()))?;
    let mut checks = checks_for(&rb);

    let reference_path: Option<PathBuf> = match args.against {
        Some(p) => Some(p),
        None => default_reference(),
    };
    let reference = if let Some(ref_path) = &reference_path {
        eprintln!("tohdr: comparing against {}", ref_path.display());
        match catch("apple-imageio", "inspect", || tohdr_apple::inspect(ref_path)) {
            Ok(ref_rb) => {
                // Pushed onto the target's list, not the reference's, because it
                // is a statement about this file and must reach the exit code.
                checks.push(reference_comparison(&rb, &ref_rb));
                Some((ref_path.display().to_string(), checks_for(&ref_rb)))
            }
            Err(e) => {
                eprintln!("tohdr: warning: could not inspect reference {}: {e:#}", ref_path.display());
                None
            }
        }
    } else {
        None
    };

    // After the reference check is in, so criterion 11 counts.
    let passed = checks.iter().all(|c| c.passed);

    let report = VerifyReport {
        file: path.display().to_string(),
        reference: reference.as_ref().map(|(p, _)| p.clone()),
        checks: checks.clone(),
        passed,
    };

    if args.json {
        println!("{}", serde_json::to_string(&report)?);
    } else {
        print_human(&report.file, &checks);
        if let Some((ref_name, ref_checks)) = &reference {
            println!();
            print_human(&format!("{ref_name} (reference)"), ref_checks);
        }
        println!();
        println!("overall: {}", if passed { "PASS" } else { "FAIL" });
    }

    Ok(if passed { 0 } else { 1 })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tohdr_core::GainMapMeta;

    fn base_readback() -> ReadBack {
        ReadBack {
            width: 100,
            height: 80,
            depth: 8,
            apple_aux: true,
            iso_aux: true,
            gain_size: Some((50, 40)),
            gain_pixel_format: Some(u32::from_be_bytes(*b"L008")),
            tag33: Some(1.0),
            tag48: Some(0.05),
            apple_headroom: Some(4.0),
            orientation: Some(1),
            iso_meta: Some(GainMapMeta::with_headroom_stops(2.287109)),
        }
    }

    #[test]
    fn healthy_file_passes_all_checks() {
        let checks = checks_for(&base_readback());
        assert!(checks.iter().all(|c| c.passed), "{checks:?}");
        for want in ["base_headroom_zero", "every_display_gets_its_stops"] {
            assert!(checks.iter().any(|c| c.name == want), "missing {want}: {checks:?}");
        }
    }

    /// Criterion 2. `gain_plane_present` used to test only that a format was
    /// *reported*, so `DSC07752_iso.heic`'s 3-channel `420f` plane — the defect
    /// the acceptance doc names for that file — read as `ok`.
    #[test]
    fn multi_channel_gain_plane_fails() {
        let mut rb = base_readback();
        rb.gain_pixel_format = Some(u32::from_be_bytes(*b"420f"));
        let checks = checks_for(&rb);
        let c = checks.iter().find(|c| c.name == "gain_plane_present").unwrap();
        assert!(!c.passed, "a 420f plane is not single-channel 8-bit");
        assert!(c.detail.contains("want L008"), "{}", c.detail);
    }

    /// Criterion 6, which this checker did not implement at all.
    #[test]
    fn non_zero_base_headroom_fails() {
        let mut rb = base_readback();
        let mut m = rb.iso_meta.unwrap();
        m.base_headroom = 1.0;
        rb.iso_meta = Some(m);
        let checks = checks_for(&rb);
        let c = checks.iter().find(|c| c.name == "base_headroom_zero").unwrap();
        assert!(!c.passed);
    }

    /// Criterion 10. The check this replaces asserted `gain_weight`'s output was
    /// within `[-1, 1]`, which `gain_weight` guarantees by construction, so it
    /// could not fail on a file whose delivered gain was wrong. Here
    /// `base_headroom = 1.0` costs a 1.0-stop display every stop it should get.
    #[test]
    fn under_delivering_file_fails_the_display_sweep() {
        let mut rb = base_readback();
        let mut m = rb.iso_meta.unwrap();
        m.base_headroom = 1.0;
        rb.iso_meta = Some(m);
        let checks = checks_for(&rb);
        let c = checks
            .iter()
            .find(|c| c.name == "every_display_gets_its_stops")
            .unwrap();
        assert!(!c.passed, "a 1.0-stop display gets nothing here: {}", c.detail);
    }

    /// Criterion 11 applies only when both files declare the same headroom;
    /// otherwise it would be comparing the two scenes.
    #[test]
    fn reference_comparison_skips_on_mismatched_headroom() {
        let rb = base_readback();
        let mut other = base_readback();
        other.iso_meta = Some(GainMapMeta::with_headroom_stops(1.0));
        let c = reference_comparison(&rb, &other);
        assert!(c.passed);
        assert!(c.detail.contains("skipped"), "{}", c.detail);
    }

    /// ...and catches the case it exists for: identical declared headroom, but
    /// we hand a display less than the reference does.
    #[test]
    fn reference_comparison_catches_a_deficit() {
        let reference = base_readback();
        let mut rb = base_readback();
        let mut m = rb.iso_meta.unwrap();
        // Same declaration, but the map starts applying a stop later, so every
        // display below alt_headroom receives less than the reference gives.
        m.base_headroom = 1.0;
        rb.iso_meta = Some(m);
        let c = reference_comparison(&rb, &reference);
        assert!(!c.passed, "{}", c.detail);
        assert!(c.detail.contains("deficit"), "{}", c.detail);
    }

    #[test]
    fn no_flavor_present_fails() {
        let mut rb = base_readback();
        rb.apple_aux = false;
        rb.iso_aux = false;
        rb.tag33 = None;
        rb.tag48 = None;
        rb.iso_meta = None;
        let checks = checks_for(&rb);
        let flavor_check = checks.iter().find(|c| c.name == "flavor_presence").unwrap();
        assert!(!flavor_check.passed);
    }

    #[test]
    fn negative_tag48_fails_the_gain_bug_check() {
        let mut rb = base_readback();
        rb.tag48 = Some(-0.008);
        let checks = checks_for(&rb);
        let tag_check = checks
            .iter()
            .find(|c| c.name == "maker_apple_tags_non_negative")
            .unwrap();
        assert!(!tag_check.passed);
        assert!(tag_check.detail.contains("toGainMapHDR"));
    }

    #[test]
    fn inconsistent_headroom_fails() {
        let mut rb = base_readback();
        // Simulate the washed-out-export defect: alt_headroom overstated
        // relative to what the plane (max_log2) actually encodes.
        let mut m = rb.iso_meta.unwrap();
        m.alt_headroom = 3.567;
        rb.iso_meta = Some(m);
        let checks = checks_for(&rb);
        let c = checks
            .iter()
            .find(|c| c.name == "headroom_consistent")
            .unwrap();
        assert!(!c.passed);
    }

    #[test]
    fn apple_only_file_does_not_require_iso_meta() {
        let mut rb = base_readback();
        rb.iso_aux = false;
        rb.iso_meta = None;
        let checks = checks_for(&rb);
        let c = checks
            .iter()
            .find(|c| c.name == "headroom_consistent")
            .unwrap();
        assert!(c.passed, "no ISO metadata to check should not fail");
    }

    /// The inverse of what this test used to assert. It required a failure when
    /// an Apple-flavor file carried no tags, which contradicted
    /// `docs/acceptance-criteria.md` §8 and `tools/verify_gainmap.py:587` — and
    /// made a conversion of any TIFF or JPEG exit non-zero, since those have no
    /// MakerNote to carry and §8's rule is "never from nothing".
    #[test]
    fn missing_tags_on_apple_flavor_skips() {
        let mut rb = base_readback();
        rb.tag33 = None;
        rb.tag48 = None;
        let checks = checks_for(&rb);
        let c = checks
            .iter()
            .find(|c| c.name == "maker_apple_tags_non_negative")
            .unwrap();
        assert!(c.passed, "no tags to check is a skip, not a failure");
        assert!(c.detail.contains("skipped"), "a skip must read as one: {}", c.detail);
        assert!(checks.iter().all(|c| c.passed), "{checks:?}");
    }

    /// tag48 alone is undecodable: tag33 selects which regime of Apple's
    /// formula applies. The catch-all arm that preceded the explicit match
    /// judged this case on `apple_aux` and let it pass.
    #[test]
    fn tag48_without_tag33_fails() {
        let mut rb = base_readback();
        rb.tag33 = None;
        let checks = checks_for(&rb);
        let c = checks
            .iter()
            .find(|c| c.name == "maker_apple_tags_non_negative")
            .unwrap();
        assert!(!c.passed, "an undecodable tag48 must not pass");
        assert!(c.detail.contains("tag33 is missing"), "{}", c.detail);
    }
}
