//! What does `primaries_from_icc` make of profiles that really exist on disk?
//!
//! `colour.rs` tests recognition against profiles built in the test itself, plus
//! the colorants copied out of the one Lightroom embeds. That proves the matching
//! arithmetic, but it cannot prove the *inputs* are right: the numbers were
//! transcribed by hand, and a profile that Lightroom or macOS writes slightly
//! differently would be missed with no test failing.
//!
//! So this feeds whole profile files through the same function. Unit tests stay
//! pure; this probe is where real bytes get read. Point it at ColorSync profiles
//! or at ICCs extracted from a TIFF's tag 34675:
//!
//! ```
//! cargo run --example probe_icc -p tohdr-core -- '/System/Library/ColorSync/Profiles/Display P3.icc'
//! ```
//!
//! Recognised means the base image gets labelled from the source. Unrecognised
//! means `convert` warns and falls back to whatever `--colour-space` asked for,
//! which is a guess — so an unrecognised profile here is a finding, not noise.

use std::path::PathBuf;

fn main() {
    let paths: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    if paths.is_empty() {
        eprintln!("usage: probe_icc <profile.icc> [...]");
        std::process::exit(2);
    }

    let mut unrecognised = 0usize;
    for path in &paths {
        let icc = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) => {
                println!("{:<28} read failed: {e}", display(path));
                unrecognised += 1;
                continue;
            }
        };
        let desc = tohdr_core::colour::icc_description(&icc).unwrap_or_else(|| "<none>".into());
        match tohdr_core::colour::primaries_from_icc(&icc) {
            Some(p) => println!(
                "{:<28} {:>5} B  desc {desc:<24} -> {} (nclx {})",
                display(path),
                icc.len(),
                p.label(),
                p.nclx()
            ),
            None => {
                unrecognised += 1;
                println!(
                    "{:<28} {:>5} B  desc {desc:<24} -> unrecognised (convert would warn)",
                    display(path),
                    icc.len()
                );
            }
        }
    }
    println!("\n{} of {} recognised", paths.len() - unrecognised, paths.len());
}

fn display(path: &std::path::Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}
