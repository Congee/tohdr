//! `tohdr batch`: a folder of sources -> a folder of gain-map HEICs.
//!
//! Not parallelism the one-file path lacks -- a single conversion already uses
//! every core. What this recovers is ImageIO's ~1.15 s of serial RAW decode, when
//! nine of ten cores idle. Overlapping files fills that hole.
//!
//! Default of four jobs, and why session pooling is on: docs/performance.md.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use serde::Serialize;

use crate::cli::BatchArgs;
use crate::convert::{convert_one, ConvertReport};

/// Extensions worth trying when an input is a directory. Deliberately a
/// whitelist: a folder of raws also holds sidecars, and handing ImageIO an
/// `.xmp` produces a confusing failure rather than a skip.
const EXTENSIONS: &[&str] = &[
    "arw", "cr2", "cr3", "dng", "nef", "orf", "raf", "rw2", "heic", "heif", "tif", "tiff", "png",
    "jpg", "jpeg",
];

/// Peak resident memory one 61 Mpx conversion reached, rounded up. Used only to
/// warn, since the real figure scales with pixel count.
const PEAK_BYTES_PER_JOB: u64 = 2_500_000_000;

#[derive(Serialize, Debug)]
struct FileOutcome {
    input: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    report: Option<ConvertReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    seconds: f64,
}

#[derive(Serialize, Debug)]
struct BatchReport {
    jobs: usize,
    converted: usize,
    failed: usize,
    seconds: f64,
    files: Vec<FileOutcome>,
}

/// Expand directories into the files inside them, one level deep, sorted.
///
/// Plain file arguments are taken as given even if their extension is not in
/// [`EXTENSIONS`] — naming a file is an explicit request, whereas naming a
/// directory is not a request for everything in it.
fn collect_inputs(inputs: &[PathBuf]) -> anyhow::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for input in inputs {
        if input.is_dir() {
            let mut found: Vec<PathBuf> = std::fs::read_dir(input)
                .map_err(|e| anyhow::anyhow!("reading directory {}: {e}", input.display()))?
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.is_file())
                .filter(|p| {
                    p.extension()
                        .and_then(|e| e.to_str())
                        .map(|e| EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
                        .unwrap_or(false)
                })
                .collect();
            found.sort();
            if found.is_empty() {
                anyhow::bail!(
                    "no convertible files in {} (looked for: {})",
                    input.display(),
                    EXTENSIONS.join(", ")
                );
            }
            out.extend(found);
        } else if input.is_file() {
            out.push(input.clone());
        } else {
            anyhow::bail!("no such file or directory: {}", input.display());
        }
    }
    if out.is_empty() {
        anyhow::bail!("no inputs");
    }
    Ok(out)
}

/// `<dir>/<stem>.heic`.
///
/// Appends rather than using `with_extension`, which replaces the last dot-run
/// of the *stem* too and would turn `a.b.c.tif` into `a.b.heic`.
fn output_for(input: &Path, dir: &Path) -> PathBuf {
    let mut name = input.file_stem().unwrap_or(input.as_os_str()).to_os_string();
    name.push(".heic");
    dir.join(name)
}

/// Reject input sets where two sources would claim one output path.
///
/// Output names come from the stem alone, so `a/DSC1.ARW` and `b/DSC1.ARW`
/// both want `<out>/DSC1.heic`. With `--jobs > 1` two workers then write that
/// file at once and the survivor is whichever finished last — silent loss of
/// one conversion. Refusing up front is the only answer that keeps the
/// input->output mapping predictable; disambiguating by hand would make the
/// name depend on directory order.
fn check_output_collisions(inputs: &[PathBuf], dir: &Path) -> anyhow::Result<()> {
    let mut claims: std::collections::HashMap<PathBuf, Vec<&PathBuf>> =
        std::collections::HashMap::new();
    for input in inputs {
        claims.entry(output_for(input, dir)).or_default().push(input);
    }
    let mut clashes: Vec<_> = claims.iter().filter(|(_, v)| v.len() > 1).collect();
    if clashes.is_empty() {
        return Ok(());
    }
    clashes.sort_by_key(|(out, _)| *out);
    let mut msg = format!("{} output name(s) claimed by more than one input:", clashes.len());
    for (out, sources) in clashes {
        msg.push_str(&format!("\n  {} <- ", out.display()));
        let names: Vec<String> = sources.iter().map(|p| p.display().to_string()).collect();
        msg.push_str(&names.join(", "));
    }
    msg.push_str("\nconvert these separately, into different --output-dir directories");
    anyhow::bail!(msg)
}

/// How many files to convert at once when `--jobs` is not given.
fn default_jobs(cores: usize) -> usize {
    cores.div_ceil(2).clamp(1, 4)
}

pub fn run(args: BatchArgs) -> anyhow::Result<i32> {
    if args.no_session_reuse {
        tohdr_apple::vtenc::set_session_reuse(false);
    }
    let inputs = collect_inputs(&args.inputs)?;
    check_output_collisions(&inputs, &args.output_dir)?;
    let cores = tohdr_core::par::available_cores();
    let jobs = args.jobs.unwrap_or_else(|| default_jobs(cores)).clamp(1, inputs.len());

    std::fs::create_dir_all(&args.output_dir).map_err(|e| {
        anyhow::anyhow!("creating output directory {}: {e}", args.output_dir.display())
    })?;

    if !args.json {
        eprintln!(
            "tohdr: {} file(s), {jobs} at a time on {cores} cores (~{:.1} GB peak)",
            inputs.len(),
            (jobs as u64 * PEAK_BYTES_PER_JOB) as f64 / 1e9,
        );
    }

    let next = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    let total = inputs.len();
    let started = Instant::now();

    // Workers pull the next index off a counter rather than taking a fixed
    // slice, because file cost varies with pixel count and a static split would
    // leave the fast worker idle. Each returns what it did; the caller reorders.
    let mut collected: Vec<(usize, FileOutcome)> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..jobs)
            .map(|_| {
                let (next, done, inputs, args) = (&next, &done, &inputs, &args);
                s.spawn(move || {
                    let mut mine = Vec::new();
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        if i >= total {
                            return mine;
                        }
                        let input = &inputs[i];
                        let one = args.convert_args_for(input, output_for(input, &args.output_dir));
                        let t = Instant::now();
                        let result = convert_one(&one, false);
                        let seconds = t.elapsed().as_secs_f64();
                        let n = done.fetch_add(1, Ordering::Relaxed) + 1;
                        let outcome = match result {
                            Ok(report) => {
                                if !args.json {
                                    eprintln!(
                                        "  [{n}/{total}] {} -> {} ({seconds:.2}s, {} bytes)",
                                        input.display(),
                                        report.output,
                                        report.bytes_written
                                    );
                                }
                                FileOutcome {
                                    input: input.display().to_string(),
                                    report: Some(report),
                                    error: None,
                                    seconds,
                                }
                            }
                            Err(e) => {
                                if !args.json {
                                    eprintln!("  [{n}/{total}] {}: FAILED: {e:#}", input.display());
                                }
                                FileOutcome {
                                    input: input.display().to_string(),
                                    report: None,
                                    error: Some(format!("{e:#}")),
                                    seconds,
                                }
                            }
                        };
                        mine.push((i, outcome));
                    }
                })
            })
            .collect();
        handles.into_iter().flat_map(|h| h.join().expect("batch worker panicked")).collect()
    });

    let seconds = started.elapsed().as_secs_f64();
    collected.sort_by_key(|(i, _)| *i);
    let files: Vec<FileOutcome> = collected.into_iter().map(|(_, f)| f).collect();
    let failed = files.iter().filter(|f| f.error.is_some()).count();
    let report = BatchReport {
        jobs,
        converted: files.len() - failed,
        failed,
        seconds,
        files,
    };

    if args.json {
        println!("{}", serde_json::to_string(&report)?);
    } else {
        let per = if report.converted > 0 {
            seconds / report.converted as f64
        } else {
            0.0
        };
        println!(
            "converted {}/{} in {seconds:.2}s ({per:.2}s per file, {:.1} files/min)",
            report.converted,
            report.converted + report.failed,
            if per > 0.0 { 60.0 / per } else { 0.0 },
        );
        for f in report.files.iter().filter(|f| f.error.is_some()) {
            println!("  failed: {} — {}", f.input, f.error.as_deref().unwrap_or(""));
        }
    }

    Ok(if report.failed > 0 { 1 } else { 0 })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_path_swaps_extension_into_the_target_dir() {
        let got = output_for(Path::new("/src/DSC07746.ARW"), Path::new("/out"));
        assert_eq!(got, PathBuf::from("/out/DSC07746.heic"));
    }

    #[test]
    fn output_path_handles_dotted_names_and_no_extension() {
        assert_eq!(
            output_for(Path::new("/src/a.b.c.tif"), Path::new("/out")),
            PathBuf::from("/out/a.b.c.heic"),
            "only the last extension is replaced"
        );
        assert_eq!(
            output_for(Path::new("/src/noext"), Path::new("/out")),
            PathBuf::from("/out/noext.heic")
        );
    }

    #[test]
    fn default_jobs_stops_at_the_measured_knee() {
        assert_eq!(default_jobs(1), 1);
        assert_eq!(default_jobs(2), 1);
        assert_eq!(default_jobs(4), 2);
        assert_eq!(default_jobs(8), 4);
        assert_eq!(default_jobs(10), 4);
        assert_eq!(default_jobs(64), 4, "more cores do not help past the knee");
    }

    #[test]
    fn directories_expand_sorted_and_filtered() {
        let dir = std::env::temp_dir().join(format!("tohdr-batch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["b.ARW", "a.arw", "notes.xmp", "c.TIF"] {
            std::fs::write(dir.join(name), b"x").unwrap();
        }
        let got = collect_inputs(std::slice::from_ref(&dir)).unwrap();
        let names: Vec<String> = got
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["a.arw", "b.ARW", "c.TIF"], "sidecar must be skipped");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_named_file_is_taken_whatever_its_extension() {
        let dir = std::env::temp_dir().join(format!("tohdr-batch-f-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let odd = dir.join("explicit.weird");
        std::fs::write(&odd, b"x").unwrap();
        assert_eq!(collect_inputs(std::slice::from_ref(&odd)).unwrap(), vec![odd]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn same_stem_in_two_directories_is_refused_before_any_work() {
        let inputs = vec![
            PathBuf::from("/shoot-a/DSC07746.ARW"),
            PathBuf::from("/shoot-b/DSC07746.arw"),
            PathBuf::from("/shoot-a/DSC07747.ARW"),
        ];
        let err = check_output_collisions(&inputs, Path::new("/out")).unwrap_err().to_string();
        assert!(err.contains("/out/DSC07746.heic"), "{err}");
        assert!(err.contains("/shoot-a/DSC07746.ARW"), "{err}");
        assert!(err.contains("/shoot-b/DSC07746.arw"), "{err}");
        assert!(!err.contains("DSC07747"), "the file that is fine must not be named: {err}");
    }

    #[test]
    fn a_differing_extension_alone_still_collides() {
        // `raw.tif` and `raw.png` are different inputs but one output name.
        let inputs = vec![PathBuf::from("/src/raw.tif"), PathBuf::from("/src/raw.png")];
        assert!(check_output_collisions(&inputs, Path::new("/out")).is_err());
    }

    #[test]
    fn distinct_stems_pass() {
        let inputs = vec![PathBuf::from("/a/one.arw"), PathBuf::from("/b/two.arw")];
        assert!(check_output_collisions(&inputs, Path::new("/out")).is_ok());
    }

    #[test]
    fn missing_input_is_an_error_not_a_skip() {
        let err = collect_inputs(&[PathBuf::from("/nope/nothing-here.arw")]).unwrap_err();
        assert!(err.to_string().contains("no such file"), "{err}");
    }
}
