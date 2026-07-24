//! `clap` argument definitions. Parsing only — no engine calls happen here,
//! which is what keeps `--help` working before any engine crate is finished.

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use tohdr_core::Flavor;
use tohdr_core::encode::parse_size;

use crate::engine::EngineKind;

#[derive(Parser, Debug)]
#[command(name = "tohdr", version, about = "Produce HDR gain-map HEICs")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Encode a gain-map HEIC from an HDR source.
    Convert(ConvertArgs),
    /// Report what's actually in a HEIC's gain map, both flavors.
    Inspect(InspectArgs),
    /// Check a file's gain map against the correctness invariants IMG_4913 holds.
    Verify(VerifyArgs),
    /// Compare the Apple and portable engines on one input.
    Bench(BenchArgs),
}

/// Which gain-map flavor(s) to write/expect.
///
/// `iso` is the canonical spelling (ISO 21496-1); `ios` is accepted as a
/// hidden alias since it is an easy typo for exactly this domain.
pub fn parse_flavor(s: &str) -> Result<Flavor, String> {
    match s.to_ascii_lowercase().as_str() {
        "apple" => Ok(Flavor::Apple),
        "iso" | "ios" => Ok(Flavor::Iso),
        "both" => Ok(Flavor::Both),
        other => Err(format!(
            "unknown flavor {other:?} (expected apple, iso, or both)"
        )),
    }
}

pub fn parse_engine(s: &str) -> Result<EngineKind, String> {
    EngineKind::parse(s)
}

/// How to render the SDR base from the HDR source. The `Reinhard` white point
/// is filled in once the source is loaded ([`crate::convert`]), so this only
/// carries the discriminant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToneMapKind {
    Clip,
    Reinhard,
}

pub fn parse_tone_map(s: &str) -> Result<ToneMapKind, String> {
    match s.to_ascii_lowercase().as_str() {
        "clip" => Ok(ToneMapKind::Clip),
        "reinhard" => Ok(ToneMapKind::Reinhard),
        other => Err(format!(
            "unknown tone-map {other:?} (expected clip or reinhard)"
        )),
    }
}

fn parse_quality(s: &str) -> Result<u8, String> {
    let v: u32 = s.parse().map_err(|_| format!("not a number: {s:?}"))?;
    if (1..=100).contains(&v) {
        Ok(v as u8)
    } else {
        Err(format!("quality must be 1..=100, got {v}"))
    }
}

#[derive(clap::Args, Debug)]
pub struct ConvertArgs {
    /// HDR source: a plain HDR file, or an existing gain-map HEIC to remux.
    pub input: PathBuf,

    /// Where to write the gain-map HEIC.
    #[arg(short = 'o', long)]
    pub output: PathBuf,

    /// Gain-map signaling to write.
    #[arg(long, default_value = "both", value_parser = parse_flavor)]
    pub flavor: Flavor,

    /// Which backend encodes and muxes.
    #[arg(long, default_value = "apple", value_parser = parse_engine)]
    pub engine: EngineKind,

    /// Target output size, e.g. `4MB`, `4MiB`, `3.5m`, `1500000`. Triggers a
    /// quality search down to `--min-quality` when the initial encode overshoots.
    #[arg(long = "max-size", value_parser = parse_size)]
    pub max_size: Option<u64>,

    /// Base-image quality, 1..=100.
    #[arg(long, default_value_t = 85, value_parser = parse_quality)]
    pub quality: u8,

    /// Floor for the `--max-size` search, 1..=100.
    #[arg(long = "min-quality", default_value_t = 40, value_parser = parse_quality)]
    pub min_quality: u8,

    /// How to render the SDR base from the HDR source.
    #[arg(long = "tone-map", default_value = "reinhard", value_parser = parse_tone_map)]
    pub tone_map: ToneMapKind,

    /// Gain-plane downscale factor relative to the base, e.g. `2` = half
    /// resolution (Apple's convention).
    #[arg(long = "gain-subsample", default_value_t = 2)]
    pub gain_subsample: u32,

    /// Override the auto-derived headroom (log2 stops). Use with care: a
    /// value that disagrees with what the gain plane actually encodes is
    /// exactly the defect documented in docs/heic-gainmap-structure.md.
    #[arg(long)]
    pub headroom: Option<f32>,

    /// Emit a machine-readable JSON result on stdout instead of text.
    #[arg(long)]
    pub json: bool,
}

#[derive(clap::Args, Debug)]
pub struct InspectArgs {
    pub file: PathBuf,

    #[arg(long)]
    pub json: bool,
}

#[derive(clap::Args, Debug)]
pub struct VerifyArgs {
    pub file: PathBuf,

    /// Reference file to compare against. Defaults to IMG_4913.HEIC, the file
    /// that renders correctly everywhere tested.
    #[arg(long = "against")]
    pub against: Option<PathBuf>,

    #[arg(long)]
    pub json: bool,
}

#[derive(clap::Args, Debug)]
pub struct BenchArgs {
    pub input: PathBuf,

    #[arg(long, default_value_t = 3)]
    pub iterations: u32,

    /// Restrict the comparison to one engine. Default is both.
    #[arg(long, value_parser = parse_engine)]
    pub engine: Option<EngineKind>,

    #[arg(long)]
    pub json: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_debug_asserts() {
        // Catches most derive-level mistakes (conflicting args, bad defaults)
        // without needing a subprocess.
        Cli::command().debug_assert();
    }

    #[test]
    fn flavor_accepts_canonical_and_alias() {
        assert_eq!(parse_flavor("apple").unwrap(), Flavor::Apple);
        assert_eq!(parse_flavor("iso").unwrap(), Flavor::Iso);
        assert_eq!(parse_flavor("ios").unwrap(), Flavor::Iso, "ios must alias iso");
        assert_eq!(parse_flavor("both").unwrap(), Flavor::Both);
        assert_eq!(parse_flavor("BOTH").unwrap(), Flavor::Both);
        assert!(parse_flavor("android").is_err());
    }

    #[test]
    fn tone_map_parses() {
        assert_eq!(parse_tone_map("clip").unwrap(), ToneMapKind::Clip);
        assert_eq!(parse_tone_map("reinhard").unwrap(), ToneMapKind::Reinhard);
        assert!(parse_tone_map("filmic").is_err());
    }

    #[test]
    fn quality_range_enforced() {
        assert_eq!(parse_quality("1").unwrap(), 1);
        assert_eq!(parse_quality("100").unwrap(), 100);
        assert!(parse_quality("0").is_err());
        assert!(parse_quality("101").is_err());
        assert!(parse_quality("abc").is_err());
    }

    #[test]
    fn convert_parses_minimal_invocation() {
        let cli = Cli::try_parse_from([
            "tohdr",
            "convert",
            "in.heic",
            "--output",
            "out.heic",
        ])
        .unwrap();
        match cli.command {
            Command::Convert(a) => {
                assert_eq!(a.flavor, Flavor::Both);
                assert_eq!(a.engine, EngineKind::Apple);
                assert_eq!(a.quality, 85);
                assert_eq!(a.min_quality, 40);
                assert_eq!(a.tone_map, ToneMapKind::Reinhard);
                assert_eq!(a.gain_subsample, 2);
                assert!(a.max_size.is_none());
                assert!(a.headroom.is_none());
                assert!(!a.json);
            }
            _ => panic!("expected Convert"),
        }
    }

    #[test]
    fn convert_parses_max_size_and_ios_alias() {
        let cli = Cli::try_parse_from([
            "tohdr",
            "convert",
            "in.heic",
            "--output",
            "out.heic",
            "--max-size",
            "4MB",
            "--flavor",
            "ios",
            "--engine",
            "portable",
        ])
        .unwrap();
        match cli.command {
            Command::Convert(a) => {
                assert_eq!(a.max_size, Some(4_000_000));
                assert_eq!(a.flavor, Flavor::Iso);
                assert_eq!(a.engine, EngineKind::Portable);
            }
            _ => panic!("expected Convert"),
        }
    }

    #[test]
    fn convert_rejects_bad_max_size() {
        let err = Cli::try_parse_from([
            "tohdr", "convert", "in.heic", "--output", "out.heic", "--max-size", "banana",
        ])
        .unwrap_err();
        assert!(err.to_string().contains("not a number"));
    }

    #[test]
    fn verify_default_against_is_none() {
        let cli = Cli::try_parse_from(["tohdr", "verify", "f.heic"]).unwrap();
        match cli.command {
            Command::Verify(a) => assert!(a.against.is_none()),
            _ => panic!("expected Verify"),
        }
    }

    #[test]
    fn bench_defaults() {
        let cli = Cli::try_parse_from(["tohdr", "bench", "in.heic"]).unwrap();
        match cli.command {
            Command::Bench(a) => {
                assert_eq!(a.iterations, 3);
                assert!(a.engine.is_none());
            }
            _ => panic!("expected Bench"),
        }
    }
}
