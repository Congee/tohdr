//! Flavour-neutral core for HDR gain maps.
//!
//! No container and no codec: the same [`GainMapMeta`] feeds every backend.
//! Engines live in sibling crates and implement [`GainMapEncoder`]. Field naming
//! follows ISO 21496-1, as libavif's `avifGainMap` also does.
//!
//! Two flavours are modelled: **ISO 21496-1** (`tmap` derived item in HEIC/AVIF,
//! `jhgm` box in JXL) and **Apple** (a
//! `urn:com:apple:photo:2020:aux:hdrgainmap` aux image plus the MakerApple
//! headroom tags -- see [`apple`]).
//!
//! [`Rgb`] is fixed-range sRGB-encoded and models the SDR base. [`hdr::HdrRgb`]
//! is linear `f32` with `1.0` at diffuse white and models the HDR source; only it
//! represents the above-white light a gain map exists to carry.

pub mod apple;
pub mod colour;
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

pub use colour::Primaries;
pub use encode::{EncodeOptions, Flavor, GainMapEncoder};
pub use orient::{heif_transform, HeifTransform};
pub use sidecar::{MetadataSupport, OpaqueItem};
pub use hdr::HdrRgb;
pub use meta::{GainMapMeta, GainPlane, Rgb};
