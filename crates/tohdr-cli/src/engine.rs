//! Selects Engine A (Apple ImageIO) or Engine B (our muxer plus a plane codec)
//! behind one type, so `convert`/`bench` can be written once against
//! [`tohdr_core::GainMapEncoder`] instead of branching on every call site.
//!
//! # Engine B has two codecs
//!
//! Engine B is [`tohdr_heif::MuxEngine`] over a [`tohdr_heif::PlaneCodec`], and
//! this is the only crate that can see both implementations, so codec selection
//! lives here. `--engine portable` means "our container, the fastest codec this
//! machine has": the media block via VideoToolbox, which is ~6x faster than the
//! software codec and beats Engine A outright, falling back to `hpvca` when the
//! request is outside what the hardware path can do.
//!
//! The choice is made **before** encoding, from the base image's bit depth and
//! the requested quality, rather than by starting a hardware encode and
//! recovering from its error. Two reasons: the two codecs produce different
//! files, so a silent post-hoc substitution would make a benchmark or a
//! reproducibility claim wrong; and the caller is told which one ran, because
//! [`Engine::name`] reports the codec rather than the flag.
//!
//! `--engine hpvca` forces the software codec — that is how the two are
//! compared, and how the portable path stays testable on a machine that has
//! hardware.

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
    Portable,
    /// Engine B pinned to the pure-Rust codec.
    Hpvca,
}

impl EngineKind {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.to_ascii_lowercase().as_str() {
            "apple" => Ok(EngineKind::Apple),
            "portable" => Ok(EngineKind::Portable),
            // "software" reads better than the codec's name at a call site that
            // only means "not the hardware one".
            "hpvca" | "software" => Ok(EngineKind::Hpvca),
            other => Err(format!(
                "unknown engine {other:?} (expected apple, portable, or hpvca)"
            )),
        }
    }
}

impl std::fmt::Display for EngineKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            EngineKind::Apple => "apple",
            EngineKind::Portable => "portable",
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
    Hardware(MuxEngine<VideoToolboxCodec>),
    /// Engine B on the pure-Rust codec.
    Portable(PortableEngine),
}

impl Engine {
    /// Build an engine without knowing what it will encode. `Portable` resolves
    /// to the hardware codec, which is right for the default 8-bit q85 job; use
    /// [`Engine::for_job`] where the base image is already in hand.
    pub fn new(kind: EngineKind) -> Self {
        match kind {
            EngineKind::Apple => Engine::Apple(AppleEngine),
            EngineKind::Portable => Engine::Hardware(MuxEngine::new(VideoToolboxCodec)),
            EngineKind::Hpvca => Engine::Portable(PortableEngine),
        }
    }

    /// Build an engine for a specific job, downgrading Engine B's codec when the
    /// hardware path cannot serve it. Returns the reason alongside, so the
    /// caller can say so.
    pub fn for_job(kind: EngineKind, base: &Rgb, base_quality: u8) -> (Self, Option<String>) {
        if kind != EngineKind::Portable {
            return (Engine::new(kind), None);
        }
        match hardware_unavailable_because(base, base_quality) {
            Some(why) => (Engine::Portable(PortableEngine), Some(why)),
            None => (Engine::new(kind), None),
        }
    }

    /// Decode a source (plain HDR file or existing gain-map HEIC) into
    /// linear extended-range HDR.
    ///
    /// Engine B reads with the pure-Rust decoders — the media block encodes, it
    /// does not decode files — with one asymmetry. The pure-Rust set is
    /// tif/tiff/png/jpg/jpeg, and a camera RAW is none of those, so before this
    /// fell back the hardware codec could not touch the format the batch path
    /// exists for:
    ///
    /// ```text
    ///   $ tohdr convert DSC07746.ARW --engine portable
    ///   error: unsupported extension Some("arw") (want tif/tiff/png/jpg/jpeg)
    /// ```
    ///
    /// Engine B is a claim about the *encoder* and the container, so pairing it
    /// with ImageIO's decoder loses nothing it was ever claiming — and
    /// [`Engine::Hardware`] is VideoToolbox, already as platform-bound as a
    /// decoder can be. [`Engine::Portable`] deliberately does *not* fall back:
    /// `--engine hpvca` is the pure-Rust reference path, and a silent hop into a
    /// system framework would make it useless as one.
    ///
    /// Only [`tohdr_portable::Error::UnsupportedInput`] falls through. A TIFF
    /// that is corrupt, or too large, must still fail as itself rather than be
    /// quietly re-decoded by a different library with different behaviour.
    /// `primaries` is the space to render *into*, and it is a lossy choice made
    /// here: ImageIO carries out-of-gamut colour as negative components and the
    /// loader clamps them, so asking for the narrow space is what discards wide
    /// colour. See [`tohdr_core::colour`].
    ///
    /// The pure-Rust decoders do not take a space — they decode a TIFF or PNG at
    /// face value — so this converts their output instead, which is exact in the
    /// widening direction and needs no clamp.
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
            Engine::Hardware(_) => catch(name, "load_hdr", || match portable(path) {
                Err(tohdr_portable::Error::UnsupportedInput(_)) => {
                    tohdr_apple::load_hdr_in(path, primaries)
                        .map_err(|e| tohdr_portable::Error::Decode(e.to_string()))
                }
                other => other,
            }),
            Engine::Portable(_) => catch(name, "load_hdr", || portable(path)),
        }
    }

    /// Decode a source's SDR rendition without applying any gain map.
    #[allow(dead_code)]
    pub fn load_sdr(&self, path: &Path) -> anyhow::Result<Rgb> {
        let name = self.name();
        match self {
            Engine::Apple(_) => catch(name, "load_sdr", || tohdr_apple::load_sdr(path)),
            Engine::Hardware(_) | Engine::Portable(_) => {
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
            Engine::Hardware(e) => e.name(),
            Engine::Portable(e) => e.name(),
        }
    }

    fn metadata_support(&self) -> tohdr_core::MetadataSupport {
        match self {
            Engine::Apple(e) => e.metadata_support(),
            Engine::Hardware(e) => e.metadata_support(),
            Engine::Portable(e) => e.metadata_support(),
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
            Engine::Hardware(e) => catch(name, "encode", || e.encode(base, gain, meta, opts)),
            Engine::Portable(e) => catch(name, "encode", || e.encode(base, gain, meta, opts)),
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
        assert_eq!(EngineKind::parse("portable").unwrap(), EngineKind::Portable);
        assert_eq!(EngineKind::parse("hpvca").unwrap(), EngineKind::Hpvca);
        assert_eq!(EngineKind::parse("software").unwrap(), EngineKind::Hpvca);
    }

    #[test]
    fn rejects_unknown_engine() {
        assert!(EngineKind::parse("cuda").is_err());
    }

    #[test]
    fn names_match_backends() {
        assert_eq!(Engine::new(EngineKind::Apple).name(), "apple-imageio");
        assert_eq!(
            Engine::new(EngineKind::Portable).name(),
            "hardware-videotoolbox"
        );
        assert_eq!(Engine::new(EngineKind::Hpvca).name(), "portable-hpvca");
    }

    #[test]
    fn eight_bit_base_at_default_quality_gets_hardware() {
        let (engine, why) = Engine::for_job(EngineKind::Portable, &base_8bit(), 85);
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
        let (engine, why) = Engine::for_job(EngineKind::Portable, &base, 85);
        assert_eq!(engine.name(), "portable-hpvca");
        assert!(why.unwrap().contains("10-bit"));
    }

    /// Asking for 4:4:4 chroma is asking for the codec that can do it.
    #[test]
    fn quality_that_wants_444_falls_back_to_the_software_codec() {
        let (engine, why) = Engine::for_job(EngineKind::Portable, &base_8bit(), 96);
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
