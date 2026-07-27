//! Selects Engine A (Apple ImageIO) or Engine B (our muxer plus a plane codec)
//! behind one type, so `convert`/`bench` can be written once against
//! [`tohdr_core::GainMapEncoder`].
//!
//! Codec selection lives here because this is the only crate that sees both
//! implementations. `--engine videotoolbox` means "our container, the platform
//! media block" -- ~6x the software codec, falling back to `hpvca` outside what
//! the hardware supports. `--engine hpvca` forces software, which is how the two
//! are compared.
//!
//! Only `hpvca` is portable; VideoToolbox is macOS-only and its path hands
//! camera RAW to ImageIO besides. Do not name it after portability.
//!
//! The choice is made *before* encoding, from bit depth and requested quality,
//! rather than by recovering from a hardware error: the two codecs produce
//! different files, so a silent substitution would invalidate a benchmark or a
//! reproducibility claim. [`Engine::name`] reports the codec, not the flag.

use std::path::Path;

use tohdr_apple::vtenc::VideoToolboxCodec;
use tohdr_apple::AppleEngine;
use tohdr_core::{EncodeOptions, GainMapEncoder, GainMapMeta, GainPlane, HdrRgb, Primaries, Rgb};
use tohdr_heif::MuxEngine;
use tohdr_portable::PortableEngine;

use crate::panic_guard::catch;

/// Which backend to use. Mirrors `--engine`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EngineKind {
    Apple,
    /// Engine B with the best plane codec available for the job.
    VideoToolbox,
    /// Engine B pinned to the pure-Rust codec.
    Hpvca,
}

impl EngineKind {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.to_ascii_lowercase().as_str() {
            "apple" => Ok(EngineKind::Apple),
            "videotoolbox" => Ok(EngineKind::VideoToolbox),
            // "software" reads better than the codec's name at a call site that
            // only means "not the hardware one".
            "hpvca" | "software" => Ok(EngineKind::Hpvca),
            other => Err(format!(
                "unknown engine {other:?} (expected apple, videotoolbox, or hpvca)"
            )),
        }
    }
}

impl std::fmt::Display for EngineKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            EngineKind::Apple => "apple",
            EngineKind::VideoToolbox => "videotoolbox",
            EngineKind::Hpvca => "hpvca",
        })
    }
}

/// Why the hardware codec was declined for a given job, if it was.
///
/// Returned rather than logged so the caller decides whether it is worth a line
/// on stderr.
pub fn hardware_unavailable_because(base: &Rgb, base_quality: u8) -> Option<String> {
    if let Err(why) = VideoToolboxCodec::supports(base) {
        return Some(why);
    }
    // Chroma is not a hardware capability check, so `supports` does not make it:
    // the software codec switches to 4:4:4 above this quality to avoid
    // subsampling loss, and the media block has no equivalent knob. Asking for
    // 4:4:4 is asking for the codec that can do it.
    if base_quality >= tohdr_portable::YUV444_QUALITY_THRESHOLD {
        return Some(format!(
            "quality {base_quality} asks for 4:4:4 chroma, the media block path is 4:2:0"
        ));
    }
    None
}

/// One concrete encoder, chosen at startup. Every call is wrapped with
/// [`catch`] so an unfinished engine reports "not yet implemented" instead of
/// panicking the whole process.
pub enum Engine {
    Apple(AppleEngine),
    /// Engine B on the platform media block.
    VideoToolbox(MuxEngine<VideoToolboxCodec>),
    /// Engine B on the pure-Rust codec.
    Hpvca(PortableEngine),
}

impl Engine {
    /// Build an engine without knowing what it will encode. `VideoToolbox`
    /// resolves to the hardware codec, which is right for the default 8-bit q85
    /// job; use [`Engine::for_job`] where the base image is already in hand.
    pub fn new(kind: EngineKind) -> Self {
        match kind {
            EngineKind::Apple => Engine::Apple(AppleEngine),
            EngineKind::VideoToolbox => Engine::VideoToolbox(MuxEngine::new(VideoToolboxCodec)),
            EngineKind::Hpvca => Engine::Hpvca(PortableEngine),
        }
    }

    /// Build an engine for a specific job, downgrading Engine B's codec when the
    /// hardware path cannot serve it. Returns the reason alongside, so the
    /// caller can say so.
    pub fn for_job(kind: EngineKind, base: &Rgb, base_quality: u8) -> (Self, Option<String>) {
        if kind != EngineKind::VideoToolbox {
            return (Engine::new(kind), None);
        }
        match hardware_unavailable_because(base, base_quality) {
            Some(why) => (Engine::Hpvca(PortableEngine), Some(why)),
            None => (Engine::new(kind), None),
        }
    }

    /// Decode a source (plain HDR file or existing gain-map HEIC) into
    /// linear extended-range HDR.
    ///
    /// Engine B reads with the pure-Rust decoders, which cover tif/png/jpg but no
    /// camera RAW -- so [`Engine::VideoToolbox`] falls back to ImageIO for those.
    /// That loses nothing it claimed (Engine B is a claim about the *encoder*, and
    /// VideoToolbox is already platform-bound), but [`Engine::Hpvca`]
    /// deliberately does not fall back: `--engine hpvca` is the pure-Rust
    /// reference path, and a silent hop into a system framework would void it.
    ///
    /// Only [`tohdr_portable::Error::UnsupportedInput`] falls through -- a corrupt
    /// or oversized TIFF must fail as itself, not be re-decoded by another library.
    ///
    /// `primaries` is the space to render *into*, and a lossy choice: ImageIO
    /// carries out-of-gamut colour as negatives and the loader clamps them. The
    /// pure-Rust decoders take no space, so their output is converted instead,
    /// which is exact in the widening direction. See [`tohdr_core::colour`].
    pub fn load_hdr(&self, path: &Path, primaries: Primaries) -> anyhow::Result<HdrRgb> {
        let name = self.name();
        let portable = |path: &Path| -> Result<HdrRgb, tohdr_portable::Error> {
            let mut hdr = tohdr_portable::load_hdr(path)?;
            // The pure-Rust loaders produce Rec.709; nothing in that set carries a
            // profile this crate reads yet, so the conversion is stated rather
            // than detected.
            tohdr_core::colour::convert_linear_rgb(&mut hdr.data, Primaries::Bt709, primaries);
            Ok(hdr)
        };
        match self {
            Engine::Apple(_) => catch(name, "load_hdr", || tohdr_apple::load_hdr_in(path, primaries)),
            Engine::VideoToolbox(_) => catch(name, "load_hdr", || match portable(path) {
                Err(tohdr_portable::Error::UnsupportedInput(_)) => {
                    tohdr_apple::load_hdr_in(path, primaries)
                        .map_err(|e| tohdr_portable::Error::Decode(e.to_string()))
                }
                other => other,
            }),
            Engine::Hpvca(_) => catch(name, "load_hdr", || portable(path)),
        }
    }

    /// Decode a source's SDR rendition without applying any gain map.
    #[allow(dead_code)]
    pub fn load_sdr(&self, path: &Path) -> anyhow::Result<Rgb> {
        let name = self.name();
        match self {
            Engine::Apple(_) => catch(name, "load_sdr", || tohdr_apple::load_sdr(path)),
            Engine::VideoToolbox(_) | Engine::Hpvca(_) => {
                catch(name, "load_sdr", || tohdr_portable::load_sdr(path))
            }
        }
    }
}

impl GainMapEncoder for Engine {
    type Error = anyhow::Error;

    fn name(&self) -> &'static str {
        match self {
            Engine::Apple(e) => e.name(),
            Engine::VideoToolbox(e) => e.name(),
            Engine::Hpvca(e) => e.name(),
        }
    }

    fn metadata_support(&self) -> tohdr_core::MetadataSupport {
        match self {
            Engine::Apple(e) => e.metadata_support(),
            Engine::VideoToolbox(e) => e.metadata_support(),
            Engine::Hpvca(e) => e.metadata_support(),
        }
    }

    fn encode(
        &self,
        base: &Rgb,
        gain: &GainPlane,
        meta: &GainMapMeta,
        opts: &EncodeOptions,
    ) -> anyhow::Result<Vec<u8>> {
        let name = self.name();
        match self {
            Engine::Apple(e) => catch(name, "encode", || e.encode(base, gain, meta, opts)),
            Engine::VideoToolbox(e) => catch(name, "encode", || e.encode(base, gain, meta, opts)),
            Engine::Hpvca(e) => catch(name, "encode", || e.encode(base, gain, meta, opts)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_8bit() -> Rgb {
        Rgb {
            width: 4,
            height: 4,
            bits: 8,
            data: vec![0u16; 4 * 4 * 3],
        }
    }

    #[test]
    fn parses_known_engines() {
        assert_eq!(EngineKind::parse("apple").unwrap(), EngineKind::Apple);
        assert_eq!(EngineKind::parse("APPLE").unwrap(), EngineKind::Apple);
        assert_eq!(
            EngineKind::parse("videotoolbox").unwrap(),
            EngineKind::VideoToolbox
        );
        assert_eq!(EngineKind::parse("hpvca").unwrap(), EngineKind::Hpvca);
        assert_eq!(EngineKind::parse("software").unwrap(), EngineKind::Hpvca);
    }

    #[test]
    fn rejects_unknown_engine() {
        assert!(EngineKind::parse("cuda").is_err());
    }

    /// The old name for the hardware engine, which described the one thing it is
    /// not. Rejected outright rather than aliased: it would otherwise keep
    /// selecting a macOS-only codec.
    #[test]
    fn rejects_the_former_portable_alias() {
        assert!(EngineKind::parse("portable").is_err());
    }

    #[test]
    fn names_match_backends() {
        assert_eq!(Engine::new(EngineKind::Apple).name(), "apple-imageio");
        assert_eq!(
            Engine::new(EngineKind::VideoToolbox).name(),
            "hardware-videotoolbox"
        );
        assert_eq!(Engine::new(EngineKind::Hpvca).name(), "portable-hpvca");
    }

    #[test]
    fn eight_bit_base_at_default_quality_gets_hardware() {
        let (engine, why) = Engine::for_job(EngineKind::VideoToolbox, &base_8bit(), 85);
        assert!(why.is_none(), "{why:?}");
        assert_eq!(engine.name(), "hardware-videotoolbox");
    }

    /// The media block's base path is 8-bit; a deeper source must not be
    /// silently truncated to reach it.
    #[test]
    fn deeper_base_falls_back_to_the_software_codec() {
        let base = Rgb {
            width: 4,
            height: 4,
            bits: 10,
            data: vec![0u16; 4 * 4 * 3],
        };
        let (engine, why) = Engine::for_job(EngineKind::VideoToolbox, &base, 85);
        assert_eq!(engine.name(), "portable-hpvca");
        assert!(why.unwrap().contains("10-bit"));
    }

    /// Asking for 4:4:4 chroma is asking for the codec that can do it.
    #[test]
    fn quality_that_wants_444_falls_back_to_the_software_codec() {
        let (engine, why) = Engine::for_job(EngineKind::VideoToolbox, &base_8bit(), 96);
        assert_eq!(engine.name(), "portable-hpvca");
        assert!(why.unwrap().contains("4:4:4"));
    }

    /// An explicit `--engine hpvca` is not second-guessed, and `--engine apple`
    /// is untouched by any of this.
    #[test]
    fn explicit_choices_are_not_overridden() {
        assert_eq!(
            Engine::for_job(EngineKind::Hpvca, &base_8bit(), 85).0.name(),
            "portable-hpvca"
        );
        assert_eq!(
            Engine::for_job(EngineKind::Apple, &base_8bit(), 96).0.name(),
            "apple-imageio"
        );
    }
}
