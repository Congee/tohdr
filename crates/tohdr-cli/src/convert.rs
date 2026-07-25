//! `tohdr convert`: HDR source -> gain-map HEIC.

use anyhow::Context;
use serde::Serialize;
use tohdr_core::derive::DeriveOptions;
use tohdr_core::encode::{encode_within_budget, EncodeOptions, GainMapEncoder};
use tohdr_core::hdr::{derive_consistent, ToneMap};

use crate::cli::{ConvertArgs, ToneMapKind};
use crate::engine::Engine;

/// Fraction of the brightest pixels ignored when picking the Reinhard white
/// point / auto headroom, matching [`tohdr_core::hdr::HdrRgb::peak_luma`]'s
/// own outlier-rejection intent. Small and fixed rather than user-facing: the
/// CLI surface only promises `--headroom` as the override knob.
const PEAK_OUTLIER_FRACTION: f64 = 0.001;

#[derive(Serialize, Debug)]
pub struct ConvertReport {
    pub input: String,
    pub output: String,
    pub engine: String,
    pub flavor: String,
    pub tone_map: String,
    pub quality: u8,
    pub gain_quality: u8,
    pub gain_subsample: u32,
    pub headroom_stops: f32,
    pub headroom_overridden: bool,
    pub bytes_written: u64,
    pub max_size: Option<u64>,
    pub attempts: u32,
    pub within_budget: Option<bool>,
}

pub fn run(args: ConvertArgs) -> anyhow::Result<i32> {
    let report = convert_one(&args, true)?;
    if args.json {
        println!("{}", serde_json::to_string(&report)?);
    } else {
        println!(
            "wrote {} ({} bytes, {} engine, {} flavor, quality {}, headroom {:.3} stops{})",
            report.output,
            report.bytes_written,
            report.engine,
            report.flavor,
            report.quality,
            report.headroom_stops,
            if report.headroom_overridden { ", overridden" } else { "" },
        );
        if let Some(max) = report.max_size {
            println!(
                "  budget: <= {max} bytes, {} attempt(s), within budget: {}",
                report.attempts,
                report.within_budget.unwrap_or(false)
            );
        }
    }
    Ok(0)
}

/// One source file to one gain-map HEIC.
///
/// Split out from [`run`] so `tohdr batch` drives the identical pipeline rather
/// than a parallel reimplementation of it. `progress` is off for batch, whose
/// workers would otherwise interleave their step lines.
pub fn convert_one(args: &ConvertArgs, progress: bool) -> anyhow::Result<ConvertReport> {
    macro_rules! step {
        ($($t:tt)*) => { if progress { eprintln!($($t)*); } };
    }

    let engine = Engine::new(args.engine);
    step!(
        "tohdr: loading {} with {} engine",
        args.input.display(),
        engine.name()
    );
    let hdr = engine
        .load_hdr(&args.input)
        .with_context(|| format!("loading HDR source {}", args.input.display()))?;

    let white = hdr.peak_luma(PEAK_OUTLIER_FRACTION);
    let tone_map = match args.tone_map {
        ToneMapKind::Clip => ToneMap::Clip,
        ToneMapKind::Reinhard => ToneMap::Reinhard { white },
    };
    step!(
        "tohdr: tone-mapping to SDR base ({:?}, peak {white:.3}x)",
        args.tone_map
    );
    let base = tone_map.to_sdr(&hdr);

    let derive_opts = DeriveOptions {
        subsample: args.gain_subsample.max(1),
        ..DeriveOptions::default()
    };
    step!("tohdr: deriving gain plane (subsample {})", derive_opts.subsample);
    let (gain, mut meta) = derive_consistent(&hdr, &base, &derive_opts);

    let derived_headroom = meta.max_log2[0];
    let mut headroom_overridden = false;
    if let Some(stops) = args.headroom {
        headroom_overridden = true;
        if (stops - derived_headroom).abs() > 1e-3 {
            eprintln!(
                "tohdr: warning: --headroom {stops:.3} overrides the derived {derived_headroom:.3} \
                 stops; a conformant renderer weights the map by (display - base) / (alt - base), \
                 so declaring more or less headroom than the plane encodes makes it under- or \
                 over-apply the map (see docs/heic-gainmap-structure.md)"
            );
        }
        meta.alt_headroom = stops;
    }

    let opts = EncodeOptions {
        flavor: args.flavor,
        base_quality: args.quality,
        gain_quality: args.quality,
    };

    let (bytes, quality_used, attempts, within_budget) = if let Some(max_bytes) = args.max_size {
        step!(
            "tohdr: searching quality for a <= {max_bytes} byte output (floor q{})",
            args.min_quality
        );
        let budgeted = encode_within_budget(
            &engine,
            &base,
            &gain,
            &meta,
            &opts,
            max_bytes,
            args.min_quality,
        )
        .context("encoding within budget")?;
        if !budgeted.within_budget {
            anyhow::bail!(
                "could not fit within {max_bytes} bytes even at the quality floor (q{}): the \
                 smallest attempt was {} bytes after {} tries. Lower --min-quality, raise \
                 --max-size, or increase --gain-subsample.",
                args.min_quality,
                budgeted.bytes.len(),
                budgeted.attempts
            );
        }
        (
            budgeted.bytes,
            budgeted.quality,
            budgeted.attempts,
            Some(true),
        )
    } else {
        step!("tohdr: encoding at quality {}", args.quality);
        let bytes = engine
            .encode(&base, &gain, &meta, &opts)
            .context("encoding")?;
        (bytes, args.quality, 1, None)
    };

    std::fs::write(&args.output, &bytes)
        .with_context(|| format!("writing {}", args.output.display()))?;

    Ok(ConvertReport {
        input: args.input.display().to_string(),
        output: args.output.display().to_string(),
        engine: engine.name().to_string(),
        flavor: format!("{:?}", args.flavor).to_ascii_lowercase(),
        tone_map: format!("{:?}", args.tone_map).to_ascii_lowercase(),
        quality: quality_used,
        gain_quality: quality_used,
        gain_subsample: derive_opts.subsample,
        headroom_stops: meta.alt_headroom,
        headroom_overridden,
        bytes_written: bytes.len() as u64,
        max_size: args.max_size,
        attempts,
        within_budget,
    })
}
