//! Selects Engine A (Apple ImageIO) or Engine B (portable hpvca) behind one
//! type, so `convert`/`bench` can be written once against
//! [`tohdr_core::GainMapEncoder`] instead of branching on every call site.

use std::path::Path;

use tohdr_apple::AppleEngine;
use tohdr_core::{EncodeOptions, GainMapEncoder, GainMapMeta, GainPlane, HdrRgb, Rgb};
use tohdr_portable::PortableEngine;

use crate::panic_guard::catch;

/// Which backend to use. Mirrors `--engine`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EngineKind {
    Apple,
    Portable,
}

impl EngineKind {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.to_ascii_lowercase().as_str() {
            "apple" => Ok(EngineKind::Apple),
            "portable" => Ok(EngineKind::Portable),
            other => Err(format!(
                "unknown engine {other:?} (expected apple or portable)"
            )),
        }
    }
}

impl std::fmt::Display for EngineKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            EngineKind::Apple => "apple",
            EngineKind::Portable => "portable",
        })
    }
}

/// One concrete encoder, chosen at startup. Every call is wrapped with
/// [`catch`] so an unfinished engine reports "not yet implemented" instead of
/// panicking the whole process.
pub enum Engine {
    Apple(AppleEngine),
    Portable(PortableEngine),
}

impl Engine {
    pub fn new(kind: EngineKind) -> Self {
        match kind {
            EngineKind::Apple => Engine::Apple(AppleEngine),
            EngineKind::Portable => Engine::Portable(PortableEngine),
        }
    }

    /// Decode a source (plain HDR file or existing gain-map HEIC) into
    /// linear extended-range HDR.
    pub fn load_hdr(&self, path: &Path) -> anyhow::Result<HdrRgb> {
        let name = self.name();
        match self {
            Engine::Apple(_) => catch(name, "load_hdr", || tohdr_apple::load_hdr(path)),
            Engine::Portable(_) => catch(name, "load_hdr", || tohdr_portable::load_hdr(path)),
        }
    }

    /// Decode a source's SDR rendition without applying any gain map.
    #[allow(dead_code)]
    pub fn load_sdr(&self, path: &Path) -> anyhow::Result<Rgb> {
        let name = self.name();
        match self {
            Engine::Apple(_) => catch(name, "load_sdr", || tohdr_apple::load_sdr(path)),
            Engine::Portable(_) => catch(name, "load_sdr", || tohdr_portable::load_sdr(path)),
        }
    }
}

impl GainMapEncoder for Engine {
    type Error = anyhow::Error;

    fn name(&self) -> &'static str {
        match self {
            Engine::Apple(e) => e.name(),
            Engine::Portable(e) => e.name(),
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
            Engine::Portable(e) => catch(name, "encode", || e.encode(base, gain, meta, opts)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_known_engines() {
        assert_eq!(EngineKind::parse("apple").unwrap(), EngineKind::Apple);
        assert_eq!(EngineKind::parse("APPLE").unwrap(), EngineKind::Apple);
        assert_eq!(EngineKind::parse("portable").unwrap(), EngineKind::Portable);
    }

    #[test]
    fn rejects_unknown_engine() {
        assert!(EngineKind::parse("cuda").is_err());
    }

    #[test]
    fn names_match_backends() {
        assert_eq!(Engine::new(EngineKind::Apple).name(), "apple-imageio");
        assert_eq!(Engine::new(EngineKind::Portable).name(), "portable-hpvca");
    }
}
