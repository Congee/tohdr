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
    /// `"lightroom-embedded"` when the source carried its own gain map and we
    /// transcoded it, `"derived"` when we computed one. The Lightroom plugin
    /// checks this: it asks Lightroom for an HDR intermediate, and if it
    /// silently got an SDR one instead, `"derived"` is the only visible
    /// symptom before the output turns out washed out.
    pub gain_map_source: String,
    /// `"none"` when the source carried no Exif, `"dropped-<engine>"` when it did
    /// and the engine cannot carry it, else where it came from — `tiff-ifd0`,
    /// `heif-exif-item`, `jpeg-app1`.
    pub exif_source: String,
    /// Metadata tags carried into the output, excluding the sub-IFD pointers.
    pub exif_tags: usize,
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
        // Printed on both paths, not just the interesting one: the Lightroom
        // plugin distinguishes them by this line, and a *missing* line then
        // means "an older binary" rather than "derived", which is the
        // difference between degrading gracefully and failing every photo.
        println!(
            "  gain map: {}",
            if report.gain_map_source == "lightroom-embedded" {
                "transcoded from the source's own, not derived"
            } else {
                "derived from the source's HDR pixels"
            }
        );
        println!(
            "  exif: {}",
            match report.exif_source.as_str() {
                "none" => "none in the source".to_string(),
                s if s.starts_with("dropped-") => {
                    format!("dropped, {} writes no Exif item", &s["dropped-".len()..])
                }
                s => format!("{} tags carried from the source ({s})", report.exif_tags),
            }
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

    // Loading only needs the *family*: which decoder reads the source. Which
    // plane codec encodes it depends on the base image, which does not exist
    // yet — that choice is made below, once it can be made from facts.
    // A Lightroom Classic "HDR Output" TIFF already contains a finished gain
    // map and Lightroom's own SDR rendition of the edit, so it needs neither a
    // decoder nor a tone-map — only the reconstruction that turns the pair back
    // into extended-range RGB. Checked before the engine's loader runs, because
    // both engines would otherwise read IFD0 alone and see an SDR image with no
    // headroom in it. See `tohdr_portable::gainmap_tiff`.
    let embedded = tohdr_portable::read_gainmap_tiff(&args.input)
        .with_context(|| format!("reading {}", args.input.display()))?;

    // Read before the pixels, and never fatal: a damaged or absent Exif block is
    // a reason to convert without metadata, not a reason to refuse the photo.
    // The source is read a second time for this, which is a few ms against a
    // decode, and buys a reader that does not have to thread through two loaders
    // and three image formats.
    let source_exif = match tohdr_portable::read_source_exif(&args.input) {
        Ok(found) => found,
        Err(e) => {
            eprintln!(
                "tohdr: warning: could not read Exif from {}: {e}; converting without it",
                args.input.display()
            );
            None
        }
    };

    let loader = Engine::new(args.engine);
    let (hdr, base, tone_map_used, gain_map_source) = match embedded {
        Some(g) => {
            step!(
                "tohdr: {} carries a Lightroom gain map, {:.4} stops declared; using its own \
                 SDR rendition as the base and skipping the tone-map",
                args.input.display(),
                g.declared.alt_headroom,
            );
            (
                g.hdr,
                g.base,
                "none-lightroom-sdr".to_string(),
                "lightroom-embedded",
            )
        }
        None => {
            step!(
                "tohdr: loading {} with {} engine",
                args.input.display(),
                loader.name()
            );
            let hdr = loader
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
            (
                hdr,
                base,
                format!("{:?}", args.tone_map).to_ascii_lowercase(),
                "derived",
            )
        }
    };

    let derive_opts = DeriveOptions {
        subsample: args.gain_subsample.max(1),
        ..DeriveOptions::default()
    };
    step!("tohdr: deriving gain plane (subsample {})", derive_opts.subsample);
    let (gain, mut meta) = derive_consistent(&hdr, &base, &derive_opts);

    // `hdr` is dead from here on and is the largest allocation in the process —
    // extended-range f32 RGB is 12 bytes/px, 689 MiB at 60 MP, against 345 MiB
    // for `base` and 14 MiB for a subsample-2 `gain`. Dropping it before the
    // encode, which allocates again for the `CGImage` and VideoToolbox, is
    // correct hygiene.
    //
    // Do not expect it to lower peak RSS: it measurably does not. Live resident
    // size (`task_info`, which can fall — not `getrusage`'s high-water mark)
    // does not move across this drop, and `malloc_zone_pressure_relief`
    // releases nothing, because macOS libmalloc marks the span
    // MADV_FREE_REUSABLE and the pages stay counted until the kernel wants
    // them. The pages *are* available to the system; RSS simply overstates it.
    // See docs/performance.md, "Memory: what one conversion actually holds".
    drop(hdr);

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

    // Now the base is in hand, so Engine B can pick its plane codec. The
    // hardware path is the default for `--engine portable`; when it cannot serve
    // this particular job the software codec takes over and says so, because the
    // two produce different files.
    let (engine, downgraded) = Engine::for_job(args.engine, &base, args.quality);
    if let Some(why) = downgraded {
        eprintln!(
            "tohdr: note: encoding with {} instead of the media block — {why}",
            engine.name()
        );
    }
    step!("tohdr: encoder is {}", engine.name());
    drop(loader);

    // Only hand the block to an engine that writes one. The distinction survives
    // into the report because "the source had no Exif" and "this engine dropped
    // it" look identical in the output file and are not the same problem.
    let carried_exif = source_exif.as_ref().filter(|_| engine.carries_exif());
    if let Some(found) = &source_exif {
        match carried_exif {
            Some(_) => step!(
                "tohdr: carrying {} Exif tags from the source ({})",
                found.tag_count,
                found.origin
            ),
            None => eprintln!(
                "tohdr: warning: {} carries {} Exif tags but the {} engine writes no Exif item, \
                 so camera, lens, exposure and date are dropped. `--engine portable` keeps them",
                args.input.display(),
                found.tag_count,
                engine.name()
            ),
        }
    }
    let (exif_source, exif_tags) = match carried_exif {
        Some(found) => (found.origin.to_string(), found.tag_count),
        None if source_exif.is_some() => (format!("dropped-{}", engine.name()), 0),
        None => ("none".to_string(), 0),
    };

    let opts = EncodeOptions {
        flavor: args.flavor,
        base_quality: args.quality,
        gain_quality: args.quality,
        exif: carried_exif.map(|e| e.tiff.as_slice()),
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
        tone_map: tone_map_used,
        gain_map_source: gain_map_source.to_string(),
        exif_source,
        exif_tags,
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
