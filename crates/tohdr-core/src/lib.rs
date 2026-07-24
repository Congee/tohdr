//! Flavor-neutral core for HDR gain maps.
//!
//! Deliberately contains no container and no codec: the same [`GainMapMeta`]
//! feeds every backend. Engines live in sibling crates and implement
//! [`GainMapEncoder`].
//!
//! Gain-map flavors this models:
//! - **ISO 21496-1** — `tmap` derived item (HEIC/AVIF), `jhgm` box (JXL)
//! - **Apple** — `urn:com:apple:photo:2020:aux:hdrgainmap` aux image plus the
//!   MakerApple headroom tags (see [`apple`])
//!
//! Field naming follows ISO 21496-1, which libavif's `avifGainMap` also tracks.

pub mod apple;
pub mod derive;
pub mod iso21496;
pub mod meta;

pub use meta::{GainMapMeta, GainPlane, Rgb};

/// A backend that can mux a base image plus a gain map into one container.
///
/// Engine A (Apple ImageIO) and Engine B (portable, hpvca-based) both implement
/// this so their outputs can be diffed byte-for-byte against each other and
/// against an iPhone reference file.
pub trait GainMapEncoder {
    type Error: core::fmt::Debug;

    /// Container/flavor label for logs and benchmark tables, e.g. `"apple-imageio"`.
    fn name(&self) -> &'static str;

    fn encode(
        &self,
        base: &Rgb,
        gain: &GainPlane,
        meta: &GainMapMeta,
    ) -> Result<Vec<u8>, Self::Error>;
}
