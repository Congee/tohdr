//! `tohdr-conformance <file.heic> [--json] [--expect-flavor apple|iso|both]`
//!
//! Exit status: 0 if every applicable criterion passes, 1 otherwise, 2 on a read
//! error, so a CI gate can branch on "unreadable" separately from "rejected".

use std::process::ExitCode;

use tohdr_conformance::{analyze, check, Check, Flavor, Info, Status};

const USAGE: &str = "usage: tohdr-conformance <file.heic> [--json] \
                     [--expect-flavor apple|iso|both]";

fn main() -> ExitCode {
    let mut path = None;
    let mut json = false;
    let mut expect = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--json" => json = true,
            "--expect-flavor" => {
                let Some(v) = args.next() else {
                    return fail(2, "--expect-flavor needs a value");
                };
                match Flavor::parse(&v) {
                    Ok(f) => expect = Some(f),
                    Err(e) => return fail(2, &e),
                }
            }
            "-h" | "--help" => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            other if other.starts_with('-') => return fail(2, &format!("unknown flag {other}")),
            other => path = Some(other.to_string()),
        }
    }
    let Some(path) = path else { return fail(2, USAGE) };

    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) => return fail(2, &format!("ERROR reading {path}: {e}")),
    };
    let info = match analyze(&bytes) {
        Ok(i) => i,
        Err(e) => return fail(2, &format!("ERROR reading {path}: {e}")),
    };

    let checks = check(&info, expect);
    if json {
        print!("{}", as_json(&path, &info, &checks));
    } else {
        report(&path, &info, &checks);
    }
    if checks.iter().any(|c| c.status == Status::Fail) {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn fail(code: u8, msg: &str) -> ExitCode {
    eprintln!("{msg}");
    ExitCode::from(code)
}

fn report(path: &str, info: &Info, checks: &[Check]) {
    println!("{path}  ({} bytes, brands: {})", info.size, info.brands.join(" "));
    match (info.base_size, info.gain_size) {
        (Some((bw, bh)), Some((gw, gh))) => {
            let frac = if gw > 0 { format!(" (1/{:.3} of base)", f64::from(bw) / f64::from(gw)) }
                       else { String::new() };
            println!("  base {bw}x{bh}   gain {gw}x{gh}{frac}");
        }
        (Some((bw, bh)), None) => println!("  base {bw}x{bh}   gain: none found"),
        _ => println!("  base: none found"),
    }
    for c in checks {
        let mark = match c.status {
            Status::Pass => "PASS",
            Status::Fail => "FAIL",
            Status::Skip => "skip",
        };
        println!("  [{mark}] {:>2}. {}\n           {}", c.criterion, c.name, c.detail);
    }
    let count = |s: Status| checks.iter().filter(|c| c.status == s).count();
    println!(
        "  => {} failed, {} passed, {} skipped",
        count(Status::Fail),
        count(Status::Pass),
        count(Status::Skip)
    );
}

fn as_json(path: &str, info: &Info, checks: &[Check]) -> String {
    let checks: Vec<_> = checks
        .iter()
        .map(|c| {
            serde_json::json!({
                "criterion": c.criterion,
                "name": c.name,
                "status": c.status.as_str(),
                "detail": c.detail,
            })
        })
        .collect();
    let failed = checks.iter().filter(|c| c["status"] == "fail").count();
    let doc = serde_json::json!({
        "info": {
            "path": path,
            "size_bytes": info.size,
            "brands": info.brands,
            "primary": info.primary,
            "gain": info.gain,
            "tmap": info.tmap,
            "base_size": info.base_size.map(|(w, h)| [w, h]),
            "gain_size": info.gain_size.map(|(w, h)| [w, h]),
            "xmp_headroom": info.xmp_headroom,
            "alt_headroom": info.iso.as_ref().map(|m| m.alternate_hdr_headroom),
            "max_log2": info.max_log2(),
        },
        "checks": checks,
        "failed": failed,
    });
    format!("{}\n", serde_json::to_string_pretty(&doc).unwrap_or_default())
}
