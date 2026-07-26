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

    checks.push(Check {
        name: "gain_plane_present".into(),
        passed: rb.gain_size.is_some() && rb.gain_pixel_format.is_some(),
        detail: match (rb.gain_size, rb.gain_pixel_format) {
            (Some((w, h)), Some(fmt)) => {
                format!("{w}x{h}, format {}", String::from_utf8_lossy(&fmt.to_be_bytes()))
            }
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

    // Absent tags are a *skip*, not a failure, even with an Apple aux image
    // present. `docs/acceptance-criteria.md` §8 settles this: writing no tags
    // is the one option that never lies, so it is what we do both when the
    // source had no MakerNote to carry and when the headroom exceeds the 3.0
    // stops Apple's formula can express. Nothing in a `ReadBack` tells those
    // apart from a writer that simply forgot — the source's MakerNote is not
    // in the output — so absence is no evidence of a defect and this check has
    // nothing to say about it. `tools/verify_gainmap.py:587` skips the same
    // case; failing it here made every TIFF and JPEG conversion exit non-zero
    // for doing the right thing.
    //
    // A skip is modeled as pass-with-reason, matching `headroom_consistent`
    // above, rather than adding a third state to `Check` and the JSON the
    // plugin reads.
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

    if let Some(m) = &rb.iso_meta {
        let phone_w = gain_weight(m, PHONE_HEADROOM_STOPS);
        let xdr_w = gain_weight(m, MAC_XDR_HEADROOM_STOPS);
        checks.push(Check {
            name: "gain_weight".into(),
            passed: (-1.0..=1.0).contains(&phone_w) && (-1.0..=1.0).contains(&xdr_w),
            detail: format!(
                "phone ({PHONE_HEADROOM_STOPS} stops): weight={phone_w:.4}; \
                 mac xdr ({MAC_XDR_HEADROOM_STOPS} stops): weight={xdr_w:.4}"
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

pub fn run(args: VerifyArgs) -> anyhow::Result<i32> {
    let path = args.file.as_path();
    eprintln!("tohdr: verifying {} via apple-imageio", path.display());
    let rb = catch("apple-imageio", "inspect", || tohdr_apple::inspect(path))
        .with_context(|| format!("inspecting {}", path.display()))?;
    let checks = checks_for(&rb);
    let passed = checks.iter().all(|c| c.passed);

    let reference_path: Option<PathBuf> = match args.against {
        Some(p) => Some(p),
        None => default_reference(),
    };
    let reference = if let Some(ref_path) = &reference_path {
        eprintln!("tohdr: comparing against {}", ref_path.display());
        match catch("apple-imageio", "inspect", || tohdr_apple::inspect(ref_path)) {
            Ok(ref_rb) => Some((ref_path.display().to_string(), checks_for(&ref_rb))),
            Err(e) => {
                eprintln!("tohdr: warning: could not inspect reference {}: {e:#}", ref_path.display());
                None
            }
        }
    } else {
        None
    };

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
        assert!(checks.iter().any(|c| c.name == "gain_weight"));
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
