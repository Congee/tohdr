//! `tohdr convert`: HDR source -> gain-map HEIC.

use anyhow::Context;
use serde::Serialize;
use tohdr_core::derive::DeriveOptions;
use tohdr_core::encode::{encode_within_budget, EncodeOptions, GainMapEncoder};
use tohdr_core::hdr::{derive_consistent, ToneMap};
use tohdr_portable::MakerNoteGraft;

use crate::cli::{ConvertArgs, ToneMapKind};
use crate::engine::Engine;

/// Byte count for a user-facing message, in the same decimal units
/// `--max-size` accepts — `4MB` parses as 4,000,000 — so a size we print can
/// be typed straight back into the flag or the export dialog's size box.
fn human_bytes(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1} MB", n as f64 / 1e6)
    } else if n >= 1_000 {
        format!("{:.0} KB", n as f64 / 1e3)
    } else {
        format!("{n} B")
    }
}

#[cfg(test)]
mod tests {
    use super::human_bytes;

    /// Decimal units, matching `--max-size`'s own parsing, so a printed size is
    /// something the user can type back into the flag verbatim.
    #[test]
    fn human_bytes_uses_the_same_units_max_size_accepts() {
        assert_eq!(human_bytes(4_000_000), "4.0 MB");
        assert_eq!(human_bytes(1_049_779), "1.0 MB");
        assert_eq!(human_bytes(900_000), "900 KB");
        assert_eq!(human_bytes(1_000), "1 KB");
        assert_eq!(human_bytes(999), "999 B");
        assert_eq!(human_bytes(0), "0 B");
    }

    /// The budget message is read off a Lightroom dialog that elides the middle
    /// of a long string, so the sizes have to stay short enough to survive at
    /// the front of it.
    #[test]
    fn human_bytes_stays_short() {
        for n in [0, 1, 999, 1_000, 999_999, 1_000_000, u64::from(u32::MAX)] {
            assert!(
                human_bytes(n).len() <= 10,
                "{n} rendered as {:?}, too long for the message",
                human_bytes(n)
            );
        }
    }
}

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
    /// What became of the source `MakerNote`'s own headroom claim: `"absent"`,
    /// `"realigned"` to this output's headroom, or `"removed"` because Apple's
    /// formula cannot express it. Reported because it is the one place a
    /// conversion rewrites a vendor's bytes rather than copying them.
    pub maker_apple_headroom: String,
    /// What became of the companion `MakerNote` named by `--maker-note-from`:
    /// `"not-offered"`, `"carried"`, or the name of the check that refused it —
    /// `"source-has-own"`, `"byte-order-differs"`, `"unreachable-offset"`,
    /// `"no-exif-ifd"`, `"dropped-<engine>"` (the backend writes no foreign
    /// `MakerNote`), `"dropped-oversize"`. Reported rather than inferred from the
    /// tag count, because a refusal and a companion that simply had no
    /// `MakerNote` look identical from outside.
    pub maker_note_graft: String,
    /// Bytes of vendor `MakerNote` taken from the companion file. `0` unless
    /// `maker_note_graft` is `"carried"`.
    pub maker_note_bytes: usize,
    /// Whether the source's XMP packet — keywords, caption, rating, rights —
    /// reached the output.
    pub xmp_carried: bool,
    /// Opaque describing items carried, by label, e.g. `["uri:metadata"]`.
    pub items_carried: Vec<String>,
    /// Describing items the source had that this engine could not write.
    pub items_dropped: Vec<String>,
    pub quality: u8,
    pub gain_quality: u8,
    pub gain_subsample: u32,
    /// Primaries the output's SDR base is in and declares, e.g. `"p3"`. On the
    /// embedded-gain-map path this is the source TIFF's own profile rather than
    /// what `--colour-space` asked for; see the note `convert` prints.
    pub colour_space: String,
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
            "wrote {} ({} bytes, {} engine, {} flavor, {} base, quality {}, \
             headroom {:.3} stops{})",
            report.output,
            report.bytes_written,
            report.engine,
            report.flavor,
            report.colour_space,
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
        // Only when a companion was named. Silence otherwise, since "no MakerNote
        // was grafted" is the answer for every conversion that never asked for one.
        if report.maker_note_graft != "not-offered" {
            println!(
                "  maker note: {}",
                if report.maker_note_bytes > 0 {
                    format!(
                        "{} bytes grafted from the camera file",
                        report.maker_note_bytes
                    )
                } else {
                    format!("not grafted ({})", report.maker_note_graft)
                }
            );
        }
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
    // The one tag a renderer cannot forward, read out of the original camera file
    // when the caller names one. Non-fatal like everything else on this road: a
    // companion that cannot be read is a reason to convert without its
    // `MakerNote`, not to refuse the photograph.
    let companion = match &args.maker_note_from {
        Some(path) => match tohdr_portable::read_maker_note(path) {
            Ok(found) => {
                if found.is_none() {
                    eprintln!(
                        "tohdr: warning: {} has no MakerNote to take, so none was grafted",
                        path.display()
                    );
                }
                found
            }
            Err(e) => {
                eprintln!(
                    "tohdr: warning: could not read a MakerNote from {}: {e}; \
                     converting without it",
                    path.display()
                );
                None
            }
        },
        None => None,
    };

    let read_exif = |companion: Option<&tohdr_portable::ForeignMakerNote>| {
        match tohdr_portable::read_source_exif_with_maker_note(&args.input, companion) {
            Ok(found) => found,
            Err(e) => {
                eprintln!(
                    "tohdr: warning: could not read Exif from {}: {e}; converting without it",
                    args.input.display()
                );
                None
            }
        }
    };
    let mut source_exif = read_exif(companion.as_ref());

    // Two reasons a graft that *worked* still has to be taken back out, both
    // decided from the backend rather than from the block.
    //
    // Read off `--engine` rather than the engine chosen below, which is not yet
    // knowable — the base image does not exist. Sound because `Engine::for_job`
    // only ever swaps Engine B's plane codec, never the backend writing metadata.
    let nominal = Engine::new(args.engine).metadata_support();
    let mut withdrawn: Option<String> = None;
    if let Some(found) = &source_exif {
        if matches!(found.maker_note_graft, MakerNoteGraft::Carried { .. }) {
            let apple = companion
                .as_ref()
                .is_some_and(tohdr_portable::ForeignMakerNote::is_apple);
            let engine_name = Engine::new(args.engine).name();
            if !nominal.maker_note && !apple {
                // Engine A rebuilds its metadata from parsed properties, and
                // ImageIO parses no vendor block but Apple's. Leaving the tag in
                // would report a graft the output does not contain.
                eprintln!(
                    "tohdr: warning: the {engine_name} engine writes only Apple's MakerNote — \
                     ImageIO has no property key for another vendor's — so the grafted block \
                     would not have reached the output and was left out. `--engine portable` \
                     writes the Exif block whole and keeps it"
                );
                withdrawn = Some(format!("dropped-{engine_name}"));
            } else if nominal.max_exif_block.is_some_and(|m| found.tiff.len() > m) {
                // An oversize block gets ImageIO to return no properties at all,
                // so the choice is one vendor block or every standard tag.
                eprintln!(
                    "tohdr: warning: the grafted MakerNote makes the Exif block {} bytes, past \
                     the {} a {engine_name} carrier can hold, and an oversize block yields no \
                     metadata at all — so it was left out and the rest of the Exif kept. \
                     `--engine portable` writes the block whole and keeps both",
                    found.tiff.len(),
                    nominal.max_exif_block.unwrap_or(0),
                );
                withdrawn = Some("dropped-oversize".to_string());
            }
        }
    }
    if withdrawn.is_some() {
        source_exif = read_exif(None);
    }

    let maker_note_graft = source_exif
        .as_ref()
        .map_or(MakerNoteGraft::NotOffered, |e| e.maker_note_graft);
    match maker_note_graft {
        MakerNoteGraft::Carried { bytes } => step!(
            "tohdr: grafted a {bytes}-byte MakerNote from the source camera file, \
             pinned at offset {}",
            companion.as_ref().map_or(0, tohdr_portable::ForeignMakerNote::offset)
        ),
        // Each refusal names the check that stopped it, because "the MakerNote is
        // missing" has six causes and only some are worth acting on.
        MakerNoteGraft::HostHasOwn => eprintln!(
            "tohdr: note: {} carries a MakerNote of its own, which was kept in preference to the \
             companion file's",
            args.input.display()
        ),
        MakerNoteGraft::ByteOrderDiffers => eprintln!(
            "tohdr: warning: the companion file's Exif is the opposite byte order to {}'s, and a \
             MakerNote carries no byte-order mark of its own, so grafting it would have \
             transposed every value it holds. It was left out",
            args.input.display()
        ),
        MakerNoteGraft::Unreachable => eprintln!(
            "tohdr: warning: the companion's MakerNote sits at an offset the rebuilt Exif block's \
             own IFDs occupy, and its contents are addressed against that offset, so it could \
             not be placed where it would still parse. It was left out"
        ),
        MakerNoteGraft::NoExifIfd => eprintln!(
            "tohdr: warning: {} has no Exif IFD to hold a MakerNote, so the companion file's was \
             left out",
            args.input.display()
        ),
        MakerNoteGraft::NotOffered => {}
    }

    // The other half of the source's metadata: its XMP packet and any item that
    // `cdsc`-describes the photograph, e.g. Apple's Photographic Styles plist.
    // Non-fatal for the same reason Exif is.
    let sidecar = match tohdr_portable::read_sidecar(&args.input) {
        Ok(found) => found,
        Err(e) => {
            eprintln!(
                "tohdr: warning: could not read XMP or metadata items from {}: {e}; \
                 converting without them",
                args.input.display()
            );
            tohdr_portable::Sidecar::default()
        }
    };

    let loader = Engine::new(args.engine);
    // One decision, two uses: the space the source is rendered into, and the space
    // the output declares. Split them and the file lies about its own pixels.
    //
    // The embedded-gain-map path is the exception, and deliberately: those base
    // pixels are *Lightroom's* rendition, shipped as they are. Re-matrixing them
    // into a different set of primaries would be reprocessing the photographer's
    // output, so that path declares what the TIFF's own ICC profile says instead
    // of what was requested.
    let mut primaries = args.colour_space;
    let (hdr, base, tone_map_used, gain_map_source) = match embedded {
        Some(g) => {
            step!(
                "tohdr: {} carries a Lightroom gain map, {:.4} stops declared; using its own \
                 SDR rendition as the base and skipping the tone-map",
                args.input.display(),
                g.declared.alt_headroom,
            );
            match g.primaries {
                Some(p) => {
                    if p != args.colour_space {
                        eprintln!(
                            "tohdr: note: {} is {} by its own ICC profile, so the output declares \
                             {} rather than the requested {}. Re-matrixing Lightroom's own SDR \
                             rendition would reprocess it; export from Lightroom in {} instead.",
                            args.input.display(),
                            p.label(),
                            p.label(),
                            args.colour_space.label(),
                            args.colour_space.label(),
                        );
                    }
                    primaries = p;
                }
                None => eprintln!(
                    "tohdr: warning: {} embeds no ICC profile this build recognises, so its \
                     colour space is a guess; declaring {}. An Adobe RGB or ProPhoto export \
                     would be silently desaturated -- export as HDR sRGB, Display P3, or \
                     Rec.2020.",
                    args.input.display(),
                    primaries.label(),
                ),
            }
            (
                g.hdr,
                g.base,
                "none-lightroom-sdr".to_string(),
                "lightroom-embedded",
            )
        }
        None => {
            step!(
                "tohdr: loading {} with {} engine, rendering into {}",
                args.input.display(),
                loader.name(),
                primaries.label()
            );
            let hdr = loader
                .load_hdr(&args.input, primaries)
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

    // The headroom is final here, and the source's MakerNote states a headroom
    // of its own — the source's, not this output's. Reconcile them before the
    // block goes anywhere, or the file ships two numbers for one quantity and
    // fails `docs/acceptance-criteria.md` §9 by construction.
    let apple_headroom_fix = match &mut source_exif {
        Some(found) => tohdr_portable::align_apple_headroom(
            &mut found.tiff,
            2f32.powf(meta.alt_headroom),
        ),
        None => tohdr_portable::AppleHeadroom::Absent,
    };
    match apple_headroom_fix {
        tohdr_portable::AppleHeadroom::Rewritten => step!(
            "tohdr: realigned the carried MakerApple headroom tags to {:.4} stops",
            meta.alt_headroom
        ),
        tohdr_portable::AppleHeadroom::Removed => eprintln!(
            "tohdr: note: dropped the carried MakerApple headroom tags — Apple's tag formula \
             cannot express {:.4} stops without understating it, and a copy that disagrees with \
             the ISO payload is worse than no copy (see docs/acceptance-criteria.md §8)",
            meta.alt_headroom
        ),
        tohdr_portable::AppleHeadroom::Absent => {}
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

    // Only hand each block to an engine that writes it. The distinction survives
    // into the report because "the source had no Exif" and "this engine dropped
    // it" look identical in the output file and are not the same problem.
    let support = engine.metadata_support();
    let carried_exif = source_exif.as_ref().filter(|_| support.exif);
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

    // XMP and the opaque items travel the same road as Exif and are reported the
    // same way. The `cdsc` filter in `tohdr_portable::sidecar` has already
    // discarded metadata belonging to auxiliary images this output does not
    // contain, so whatever is here genuinely describes this photograph.
    let carried_xmp = sidecar.xmp.as_deref().filter(|_| support.xmp);
    let carried_items: &[tohdr_core::OpaqueItem] = if support.opaque_items {
        &sidecar.items
    } else {
        &[]
    };
    if sidecar.xmp.is_some() && carried_xmp.is_none() {
        eprintln!(
            "tohdr: warning: {} carries an XMP packet but the {} engine writes none, so \
             keywords, caption, rating and rights are dropped",
            args.input.display(),
            engine.name()
        );
    }
    if !sidecar.items.is_empty() {
        let labels = sidecar
            .items
            .iter()
            .map(tohdr_core::OpaqueItem::label)
            .collect::<Vec<_>>()
            .join(", ");
        if carried_items.is_empty() {
            eprintln!(
                "tohdr: warning: {} carries {} describing item(s) ({labels}) that the {} engine \
                 cannot write — ImageIO exposes no way to add one. `--engine portable` keeps them",
                args.input.display(),
                sidecar.items.len(),
                engine.name()
            );
        } else {
            step!(
                "tohdr: carrying {} describing item(s) from the source ({labels})",
                carried_items.len()
            );
        }
    }
    if carried_xmp.is_some() {
        step!("tohdr: carrying the source's XMP packet, merged with our headroom");
    }
    // The Exif block always carries tag 33723, so this is only about the second
    // copy a JPEG-carrier backend needs — and about telling the truth when that
    // backend's own writer drops it anyway.
    if !support.iptc && carried_exif.is_some_and(|e| e.iptc.is_some()) {
        eprintln!(
            "tohdr: warning: {} carries an IPTC-IIM block that the {} engine's writer emits no \
             IPTC for, so creator, rights and keywords survive only where the source also put \
             them in XMP. `--engine portable` keeps the block itself",
            args.input.display(),
            engine.name()
        );
    }

    let opts = EncodeOptions {
        flavor: args.flavor,
        base_quality: args.quality,
        gain_quality: args.quality,
        exif: carried_exif.map(|e| e.tiff.as_slice()),
        iptc: carried_exif
            .filter(|_| support.iptc)
            .and_then(|e| e.iptc.as_deref()),
        xmp: carried_xmp,
        opaque_items: carried_items,
        orientation: tohdr_core::heif_transform(
            source_exif.as_ref().map_or(1, |e| e.orientation),
        ),
        base_primaries: primaries,
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
            // Both sizes go first, deliberately. Lightroom's Export Results
            // dialog elides the *middle* of a long message, so anything past
            // the opening clause may never reach the person who set the
            // budget -- the old wording put every number exactly there and
            // showed them "could not fit within..., raise --max-size".
            anyhow::bail!(
                "could not fit: needs {}, limit {} (smallest at the q{} floor, {} tries). \
                 Raise --max-size, lower --min-quality, or increase --gain-subsample.",
                human_bytes(budgeted.bytes.len() as u64),
                human_bytes(max_bytes),
                args.min_quality,
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
        maker_apple_headroom: match apple_headroom_fix {
            tohdr_portable::AppleHeadroom::Absent => "absent",
            tohdr_portable::AppleHeadroom::Rewritten => "realigned",
            tohdr_portable::AppleHeadroom::Removed => "removed",
        }
        .to_string(),
        // `withdrawn` outranks the block's own answer: after the re-read the block
        // honestly says no graft was offered, which is not what happened.
        maker_note_graft: withdrawn.unwrap_or_else(|| maker_note_graft.to_string()),
        maker_note_bytes: match maker_note_graft {
            MakerNoteGraft::Carried { bytes } => bytes,
            _ => 0,
        },
        xmp_carried: carried_xmp.is_some(),
        items_carried: carried_items.iter().map(|i| i.label()).collect(),
        items_dropped: if carried_items.is_empty() {
            sidecar.items.iter().map(|i| i.label()).collect()
        } else {
            Vec::new()
        },
        quality: quality_used,
        gain_quality: quality_used,
        gain_subsample: derive_opts.subsample,
        colour_space: primaries.label().to_string(),
        headroom_stops: meta.alt_headroom,
        headroom_overridden,
        bytes_written: bytes.len() as u64,
        max_size: args.max_size,
        attempts,
        within_budget,
    })
}
