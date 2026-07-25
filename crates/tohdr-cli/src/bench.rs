//! `tohdr bench`: compare the Apple and portable engines on one input —
//! wall time and output size.
//!
//! The source is loaded and the gain map derived **once**, and both engines
//! encode those identical bytes. Letting each engine load the source itself
//! would fold two different TIFF decoders into the measurement, so the
//! numbers would no longer isolate the thing being compared. The load and
//! derive cost is reported separately rather than hidden.
//!
//! # First iteration versus the rest
//!
//! Repeating an encode is not the same as doing it once, and on the hardware
//! codec the difference is large: the first encode of a given geometry creates a
//! `VTCompressionSession` and brings the media block's pipeline up, and both are
//! then reused (`tohdr_apple::vtenc`'s session pool). Measured at 12.19 MP, the
//! base plane costs 97.1 ms the first time and 27.5 ms after — so a mean over
//! iterations answers "what does `tohdr batch` get per file", while the first
//! iteration alone answers "what does one `tohdr convert` in a cold process
//! get". Both are reported, because collapsing them into one mean would
//! flatter or slander the engine depending only on `--iterations`.
//!
//! `--no-session-reuse` forces every iteration to be a first iteration, which is
//! how the pool's worth is measured.

use std::time::{Duration, Instant};

use serde::Serialize;
use tohdr_core::derive::DeriveOptions;
use tohdr_core::encode::{EncodeOptions, GainMapEncoder};
use tohdr_core::hdr::{derive_consistent, ToneMap};

use crate::cli::BenchArgs;
use crate::engine::{Engine, EngineKind};

const PEAK_OUTLIER_FRACTION: f64 = 0.001;

#[derive(Serialize, Debug)]
struct EngineResult {
    engine: String,
    iterations_run: u32,
    iterations_ok: u32,
    mean_millis: Option<f64>,
    min_millis: Option<f64>,
    max_millis: Option<f64>,
    /// The first iteration on its own — a cold process's cost, before any
    /// session pooling or framework warm-up has happened.
    first_millis: Option<f64>,
    /// Mean of iterations 2..n, i.e. what a batch gets per file. `None` with
    /// `--iterations 1`, where there is no such thing.
    warm_mean_millis: Option<f64>,
    output_bytes: Option<u64>,
    error: Option<String>,
}

#[derive(Serialize, Debug)]
struct BenchReport {
    input: String,
    width: u32,
    height: u32,
    megapixels: f64,
    /// One-time cost of decoding the source and deriving the gain map, shared
    /// by both engines and therefore excluded from their per-engine timings.
    load_and_derive_millis: f64,
    engines: Vec<EngineResult>,
}

fn bench_one(
    engine_kind: EngineKind,
    base: &tohdr_core::Rgb,
    gain: &tohdr_core::GainPlane,
    meta: &tohdr_core::GainMapMeta,
    iterations: u32,
) -> EngineResult {
    let opts = EncodeOptions::default();
    // `for_job`, not `new`: a benchmark must be labelled with the codec that ran.
    // If the hardware path cannot serve this base, the row says `portable-hpvca`
    // rather than claiming a hardware number.
    let (engine, downgraded) = Engine::for_job(engine_kind, base, opts.base_quality);
    if let Some(why) = downgraded {
        eprintln!("tohdr: {engine_kind} falls back to {} — {why}", engine.name());
    }
    let name = engine.name().to_string();

    let mut durations = Vec::with_capacity(iterations as usize);
    let mut last_bytes = None;
    let mut last_error = None;
    for _ in 0..iterations.max(1) {
        let start = Instant::now();
        match engine.encode(base, gain, meta, &opts) {
            Ok(bytes) => {
                durations.push(start.elapsed());
                last_bytes = Some(bytes.len() as u64);
            }
            Err(e) => {
                last_error = Some(e.to_string());
                break;
            }
        }
    }

    if durations.is_empty() {
        return EngineResult {
            engine: name,
            iterations_run: iterations.max(1),
            iterations_ok: 0,
            mean_millis: None,
            min_millis: None,
            max_millis: None,
            first_millis: None,
            warm_mean_millis: None,
            output_bytes: None,
            error: last_error,
        };
    }

    let total: Duration = durations.iter().sum();
    let mean = total.as_secs_f64() * 1000.0 / durations.len() as f64;
    let min = durations.iter().min().unwrap().as_secs_f64() * 1000.0;
    let max = durations.iter().max().unwrap().as_secs_f64() * 1000.0;
    let first = durations[0].as_secs_f64() * 1000.0;
    let warm_mean = if durations.len() > 1 {
        let rest: Duration = durations[1..].iter().sum();
        Some(rest.as_secs_f64() * 1000.0 / (durations.len() - 1) as f64)
    } else {
        None
    };

    EngineResult {
        engine: name,
        iterations_run: iterations.max(1),
        iterations_ok: durations.len() as u32,
        mean_millis: Some(mean),
        min_millis: Some(min),
        max_millis: Some(max),
        first_millis: Some(first),
        warm_mean_millis: warm_mean,
        output_bytes: last_bytes,
        error: last_error,
    }
}

pub fn run(args: BenchArgs) -> anyhow::Result<i32> {
    if args.no_session_reuse {
        // Turning it off also empties the pool, so this really is cold every
        // iteration and not just from here on.
        tohdr_apple::vtenc::set_session_reuse(false);
        eprintln!("tohdr: session pooling off — every iteration creates its own encoder");
    }

    // All three by default: Engine A, Engine B on the media block, and Engine B
    // on the software codec. The third is the slow one, but leaving it out would
    // hide what the hardware codec is being compared against — the whole point
    // of the table in docs/engine-comparison.md.
    let kinds = match args.engine {
        Some(k) => vec![k],
        None => vec![EngineKind::Apple, EngineKind::Portable, EngineKind::Hpvca],
    };

    // Decode with the portable path specifically: it is the deterministic
    // one, and using either engine's own decoder here would privilege that
    // engine's notion of the input.
    let prep = Instant::now();
    let hdr = tohdr_portable::load_hdr(&args.input)
        .map_err(|e| anyhow::anyhow!("loading {}: {e}", args.input.display()))?;
    let white = hdr.peak_luma(PEAK_OUTLIER_FRACTION);
    let base = ToneMap::Reinhard { white }.to_sdr(&hdr);
    let (gain, meta) = derive_consistent(&hdr, &base, &DeriveOptions::default());
    let load_and_derive_millis = prep.elapsed().as_secs_f64() * 1000.0;
    eprintln!(
        "tohdr: {}x{} source, load+derive {:.1}ms (shared by both engines)",
        hdr.width, hdr.height, load_and_derive_millis
    );

    let mut results = Vec::new();
    for kind in kinds {
        eprintln!("tohdr: benchmarking {kind} ({} iterations)", args.iterations);
        results.push(bench_one(kind, &base, &gain, &meta, args.iterations));
    }

    let all_failed = results.iter().all(|r| r.iterations_ok == 0);

    let report = BenchReport {
        input: args.input.display().to_string(),
        width: hdr.width,
        height: hdr.height,
        megapixels: (hdr.width as f64 * hdr.height as f64) / 1.0e6,
        load_and_derive_millis,
        engines: results,
    };

    if args.json {
        println!("{}", serde_json::to_string(&report)?);
    } else {
        println!(
            "bench: {} ({}x{}, {:.2} MP)",
            report.input, report.width, report.height, report.megapixels
        );
        println!(
            "  shared load+derive: {:.2}ms (excluded from the per-engine numbers)",
            report.load_and_derive_millis
        );
        for r in &report.engines {
            match r.mean_millis {
                Some(mean) => println!(
                    "  {}: {}/{} ok, mean {:.2}ms (min {:.2}ms, max {:.2}ms), \
                     first {:.2}ms{}, {} bytes, {:.1} MP/s",
                    r.engine,
                    r.iterations_ok,
                    r.iterations_run,
                    mean,
                    r.min_millis.unwrap_or(0.0),
                    r.max_millis.unwrap_or(0.0),
                    r.first_millis.unwrap_or(0.0),
                    match r.warm_mean_millis {
                        Some(w) => format!(", then {w:.2}ms"),
                        None => String::new(),
                    },
                    r.output_bytes.unwrap_or(0),
                    report.megapixels / (mean / 1000.0),
                ),
                None => println!(
                    "  {}: 0/{} ok -- {}",
                    r.engine,
                    r.iterations_run,
                    r.error.as_deref().unwrap_or("unknown error")
                ),
            }
        }
    }

    Ok(if all_failed { 1 } else { 0 })
}
