//! What does `primaries_from_icc` make of profiles that really exist on disk?
//!
//! `colour.rs`'s tests build their profiles in-test from hand-transcribed
//! colorants, which proves the matching arithmetic but not the inputs -- a profile
//! macOS writes slightly differently would be missed with no test failing. So this
//! feeds whole profile files through the same function, keeping the unit tests
//! pure.
//!
//! Unrecognised is a finding, not noise: it means `convert` warns and falls back to
//! whatever `--colour-space` guessed.
//!
//! ```
//! cargo run --example probe_icc -p tohdr-core -- '/System/Library/ColorSync/Profiles/Display P3.icc'
//! ```

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
