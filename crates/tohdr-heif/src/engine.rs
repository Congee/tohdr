//! Engine B assembled from a swappable plane encoder plus [`crate::mux`].
//!
//! The muxer is the part of this pipeline worth owning; the HEVC encoder is not.
//! Profiling a 60 MP conversion put 23.3 of 30.8 CPU-seconds inside the software
//! codec and ~0.1 ms inside `mux` (`docs/engine-comparison.md`), so the encoder
//! is the entire performance story and the muxer is indifferent to which one
//! produced its input. [`PlaneCodec`] is that seam: the platform media block
//! (VideoToolbox today; VA-API or Vulkan Video on other platforms) and the
//! pure-Rust fallback are interchangeable behind it, and [`MuxEngine`] is the
//! container half that neither of them has to reimplement.

use tohdr_core::{EncodeOptions, GainMapEncoder, GainMapMeta, GainPlane, Rgb};

use crate::{Chroma, CodedImage, ColourInfo, MuxRequest};

/// A HEVC encoder for the two planes a gain-map HEIC holds.
///
/// Implementors must return **one coded image item**, never a HEIF `grid`:
/// [`crate::HeifFile::coded_image`] refuses a grid because reassembling tiles
/// into a single bitstream is a re-encode, not a remux. That constraint is what
/// rules out several otherwise-obvious encoder entry points — see
/// `tohdr_portable`'s codec module for the one that bit us.
pub trait PlaneCodec {
    /// `Send` because [`MuxEngine::encode`] encodes the two planes on separate
    /// threads and joins their results.
    type Error: core::fmt::Display + Send;

    /// Identifies the backend in `--engine` output and benchmark reports.
    fn name(&self) -> &'static str;

    /// The `colr` that describes what [`PlaneCodec::encode_base`] *actually
    /// produced*.
    ///
    /// # Why this is the codec's business and not the muxer's
    ///
    /// An encoder fed RGB picks its own RGB→YCbCr matrix, and the container has
    /// to declare the one that was used — a decoder applies the inverse of
    /// whatever the `colr` claims. Get it wrong and every pixel decodes through
    /// the wrong matrix: a *constant* error, immune to bitrate.
    ///
    /// This is not hypothetical. The muxer used to hard-code BT.601 for both
    /// backends, which is right for one of them and silently wrong for the other.
    /// Measured on a 12 MP photograph, one encode per codec, varying only this
    /// declaration (`tohdr-apple/examples/probe_vt_colour.rs`):
    ///
    /// ```text
    ///                    declared BT.709   declared BT.601
    ///   VideoToolbox         70.00 dB          49.04 dB
    ///   hpvca                51.81 dB          69.31 dB
    /// ```
    ///
    /// 21 dB, and it hid behind a *smaller* file — which reads like a win until
    /// the reconstruction is measured. Hence no default implementation: a new
    /// backend must state its matrix rather than inherit someone else's.
    fn base_colour(&self) -> ColourInfo;

    /// Encode the SDR base.
    fn encode_base(&self, base: &Rgb, quality: u8) -> Result<CodedImage, Self::Error>;

    /// Encode the gain plane. ISO 21496-1 and every Apple gain plane measured
    /// are single-channel 8-bit, so this should come back
    /// [`Chroma::Monochrome`].
    fn encode_gain(&self, gain: &GainPlane, quality: u8) -> Result<CodedImage, Self::Error>;
}

/// What went wrong while turning two planes into a file.
#[derive(Debug)]
pub enum MuxEngineError {
    /// The plane codec failed. The string is the codec's own message, since
    /// each backend has its own error type.
    Encode { plane: &'static str, message: String },
    Mux(crate::Error),
}

impl core::fmt::Display for MuxEngineError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            MuxEngineError::Encode { plane, message } => write!(f, "{plane} encode: {message}"),
            MuxEngineError::Mux(e) => write!(f, "mux: {e}"),
        }
    }
}

impl std::error::Error for MuxEngineError {}

impl From<crate::Error> for MuxEngineError {
    fn from(e: crate::Error) -> Self {
        MuxEngineError::Mux(e)
    }
}

/// Engine B: any [`PlaneCodec`] plus our muxer.
#[derive(Debug, Default, Clone, Copy)]
pub struct MuxEngine<C>(pub C);

impl<C> MuxEngine<C> {
    pub const fn new(codec: C) -> Self {
        Self(codec)
    }
}

impl<C: PlaneCodec + Sync> GainMapEncoder for MuxEngine<C> {
    type Error = MuxEngineError;

    fn name(&self) -> &'static str {
        self.0.name()
    }

    fn encode(
        &self,
        base: &Rgb,
        gain: &GainPlane,
        meta: &GainMapMeta,
        opts: &EncodeOptions,
    ) -> Result<Vec<u8>, MuxEngineError> {
        // The two planes are independent encodes and neither saturates the
        // machine on its own, so overlapping them costs less than their sum.
        // True for both backends, for different reasons: the software codec's
        // WPP parallelism is bounded by CTU rows and leaves cores idle in its
        // tail (1087 -> 982 ms for a 60 MP pair), and the hardware path pays a
        // per-geometry start-up cost — session creation plus VideoToolbox's lazy
        // first-frame bring-up — that one plane can pay while the other encodes
        // (803 -> 561 ms).
        //
        // Note the two planes are never the same session on the hardware path:
        // different dimensions, and mono against colour. So a pool cannot serve
        // them from one entry, and this thread is not contending for one either.
        let (base_coded, gain_coded) = std::thread::scope(|s| {
            let g = s.spawn(|| self.0.encode_gain(gain, opts.gain_quality));
            let b = self.0.encode_base(base, opts.base_quality);
            (b, g.join().expect("gain encode thread panicked"))
        });
        let base_coded = base_coded.map_err(|e| MuxEngineError::Encode {
            plane: "base",
            message: e.to_string(),
        })?;
        let gain_coded = gain_coded.map_err(|e| MuxEngineError::Encode {
            plane: "gain",
            message: e.to_string(),
        })?;

        let req = MuxRequest {
            base: base_coded,
            gain: gain_coded,
            meta: *meta,
            flavor: opts.flavor,
            base_colour: Some(self.0.base_colour()),
            // The `tmap` describes the reconstructed HDR image, not the SDR
            // base: Display P3 primaries with the PQ transfer, which is what
            // `IMG_4913.HEIC` puts here (as an ICC profile rather than
            // `nclx`).
            tmap_colour: Some(ColourInfo::Nclx {
                primaries: 12, // Display P3
                transfer: 16,  // SMPTE ST 2084 (PQ)
                matrix: 6,
                full_range: true,
            }),
            exif: None,
            // Apple writes the headroom three times and all three agree; a
            // consumer reading the XMP copy rather than the tmap must not get
            // a different number. Only emitted for flavors that claim Apple
            // compatibility, since it is Apple's namespace.
            xmp: opts
                .flavor
                .writes_apple()
                .then(|| tohdr_core::xmp::headroom_packet(meta.alt_headroom)),
            clli: None,
        };
        Ok(crate::mux(&req)?)
    }
}

/// Pull the coded image back out of a self-contained single-item HEIC.
///
/// Some encoders hand back a whole HEIF file rather than a bare bitstream. That
/// container is not the multi-item gain-map file we are building, so the coded
/// HEVC and its `hvcC` have to come back out before [`crate::mux`] can place
/// them alongside the other plane. The parse is ~0.05 ms on a 60 MP item — it
/// walks boxes and slices, it does not decode pixels.
pub fn coded_image_from_heic(heic: &[u8], plane: &'static str) -> Result<CodedImage, MuxEngineError> {
    let file = crate::HeifFile::parse(heic)?;
    let item = file.primary_item().ok_or_else(|| MuxEngineError::Encode {
        plane,
        message: "encoder output has no primary item".into(),
    })?;
    Ok(file.coded_image(item)?)
}

/// Chroma sampling for a plane the codec describes only as mono-or-not.
pub const fn chroma_for(monochrome: bool) -> Chroma {
    if monochrome {
        Chroma::Monochrome
    } else {
        Chroma::Yuv420
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A codec that fails on demand, so the error path is covered without
    /// pulling a real HEVC encoder into this crate's tests.
    struct Failing {
        fail_base: bool,
    }

    impl PlaneCodec for Failing {
        type Error = String;

        fn name(&self) -> &'static str {
            "failing-test-codec"
        }

        fn base_colour(&self) -> ColourInfo {
            ColourInfo::Nclx {
                primaries: 1,
                transfer: 13,
                matrix: 6,
                full_range: true,
            }
        }

        fn encode_base(&self, _base: &Rgb, _quality: u8) -> Result<CodedImage, String> {
            if self.fail_base {
                Err("base is broken".into())
            } else {
                Ok(stub(4, 4, Chroma::Yuv420))
            }
        }

        fn encode_gain(&self, _gain: &GainPlane, _quality: u8) -> Result<CodedImage, String> {
            Err("gain is broken".into())
        }
    }

    fn stub(width: u32, height: u32, chroma: Chroma) -> CodedImage {
        CodedImage {
            width,
            height,
            bit_depth: 8,
            chroma,
            hvcc: Vec::new(),
            data: Vec::new(),
        }
    }

    fn inputs() -> (Rgb, GainPlane, GainMapMeta) {
        (
            Rgb {
                width: 4,
                height: 4,
                bits: 8,
                data: vec![0u16; 4 * 4 * 3],
            },
            GainPlane {
                width: 2,
                height: 2,
                data: vec![128u8; 4],
            },
            GainMapMeta::default(),
        )
    }

    #[test]
    fn engine_takes_its_name_from_the_codec() {
        assert_eq!(
            MuxEngine::new(Failing { fail_base: false }).name(),
            "failing-test-codec"
        );
    }

    #[test]
    fn base_failure_names_the_base_plane() {
        let (base, gain, meta) = inputs();
        let err = MuxEngine::new(Failing { fail_base: true })
            .encode(&base, &gain, &meta, &EncodeOptions::default())
            .unwrap_err()
            .to_string();
        assert!(err.contains("base encode"), "{err}");
        assert!(err.contains("base is broken"), "{err}");
    }

    /// A panic in the gain thread must not be reported as a base-plane
    /// failure, and a gain error must survive the join.
    #[test]
    fn gain_failure_names_the_gain_plane() {
        let (base, gain, meta) = inputs();
        let err = MuxEngine::new(Failing { fail_base: false })
            .encode(&base, &gain, &meta, &EncodeOptions::default())
            .unwrap_err()
            .to_string();
        assert!(err.contains("gain encode"), "{err}");
        assert!(err.contains("gain is broken"), "{err}");
    }

    #[test]
    fn chroma_follows_the_monochrome_flag() {
        assert_eq!(chroma_for(true), Chroma::Monochrome);
        assert_eq!(chroma_for(false), Chroma::Yuv420);
    }

    #[test]
    fn coded_image_from_garbage_is_an_error_not_a_panic() {
        assert!(coded_image_from_heic(b"not a heic file at all", "base").is_err());
    }
}
