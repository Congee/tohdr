//! `tohdr bench`: compare the Apple and portable engines on one input —
//! wall time and output size, same base/plane/metadata for both so the
//! comparison is apples-to-apples (pun intended).

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
    output_bytes: Option<u64>,
    error: Option<String>,
}

#[derive(Serialize, Debug)]
struct BenchReport {
    input: String,
    engines: Vec<EngineResult>,
}

fn bench_one(engine_kind: EngineKind, input: &std::path::Path, iterations: u32) -> EngineResult {
    let engine = Engine::new(engine_kind);
    let name = engine.name().to_string();

    let hdr = match engine.load_hdr(input) {
        Ok(h) => h,
        Err(e) => {
            return EngineResult {
                engine: name,
                iterations_run: 0,
                iterations_ok: 0,
                mean_millis: None,
                min_millis: None,
                max_millis: None,
                output_bytes: None,
                error: Some(e.to_string()),
            }
        }
    };

    let white = hdr.peak_luma(PEAK_OUTLIER_FRACTION);
    let tone_map = ToneMap::Reinhard { white };
    let base = tone_map.to_sdr(&hdr);
    let (gain, meta) = derive_consistent(&hdr, &base, &DeriveOptions::default());
    let opts = EncodeOptions::default();

    let mut durations = Vec::with_capacity(iterations as usize);
    let mut last_bytes = None;
    let mut last_error = None;
    for _ in 0..iterations.max(1) {
        let start = Instant::now();
        match engine.encode(&base, &gain, &meta, &opts) {
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
            output_bytes: None,
            error: last_error,
        };
    }

    let total: Duration = durations.iter().sum();
    let mean = total.as_secs_f64() * 1000.0 / durations.len() as f64;
    let min = durations.iter().min().unwrap().as_secs_f64() * 1000.0;
    let max = durations.iter().max().unwrap().as_secs_f64() * 1000.0;

    EngineResult {
        engine: name,
        iterations_run: iterations.max(1),
        iterations_ok: durations.len() as u32,
        mean_millis: Some(mean),
        min_millis: Some(min),
        max_millis: Some(max),
        output_bytes: last_bytes,
        error: last_error,
    }
}

pub fn run(args: BenchArgs) -> anyhow::Result<i32> {
    let kinds = match args.engine {
        Some(k) => vec![k],
        None => vec![EngineKind::Apple, EngineKind::Portable],
    };

    let mut results = Vec::new();
    for kind in kinds {
        eprintln!("tohdr: benchmarking {kind} ({} iterations)", args.iterations);
        results.push(bench_one(kind, &args.input, args.iterations));
    }

    let all_failed = results.iter().all(|r| r.iterations_ok == 0);

    let report = BenchReport {
        input: args.input.display().to_string(),
        engines: results,
    };

    if args.json {
        println!("{}", serde_json::to_string(&report)?);
    } else {
        println!("bench: {}", report.input);
        for r in &report.engines {
            match r.mean_millis {
                Some(mean) => println!(
                    "  {}: {}/{} ok, mean {:.2}ms (min {:.2}ms, max {:.2}ms), {} bytes",
                    r.engine,
                    r.iterations_ok,
                    r.iterations_run,
                    mean,
                    r.min_millis.unwrap_or(0.0),
                    r.max_millis.unwrap_or(0.0),
                    r.output_bytes.unwrap_or(0),
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
