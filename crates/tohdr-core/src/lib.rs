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
//!
//! # Pixel types
//!
//! [`Rgb`] is fixed-range (`0..=max_value`, sRGB-encoded) and models the **SDR
//! base**. [`hdr::HdrRgb`] is linear `f32` with `1.0` at SDR diffuse white and
//! models the **HDR source**; only it can represent the above-white light a gain
//! map exists to carry.

pub mod apple;
pub mod derive;
pub mod encode;
pub mod exif;
pub mod hdr;
pub mod par;
pub mod xmp;
pub mod iso21496;
pub mod meta;
pub mod orient;
pub mod sidecar;

pub use encode::{EncodeOptions, Flavor, GainMapEncoder};
pub use orient::{heif_transform, HeifTransform};
pub use sidecar::{MetadataSupport, OpaqueItem};
pub use hdr::HdrRgb;
pub use meta::{GainMapMeta, GainPlane, Rgb};
