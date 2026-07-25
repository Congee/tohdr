//! Does the media block reach Engine A's *fidelity*, and what does that cost?
//!
//! `probe_vt_tuning.rs` swept quality and `RealTime` on bytes and milliseconds
//! and found `RealTime=true` both faster and smaller — from which it is tempting
//! to conclude there is no trade. That conclusion does not follow: fewer bytes at
//! the same requested quality is exactly what a *lower-fidelity* encode looks
//! like. `examples/roundtrip` then measured the reconstruction and put the
//! hardware path at 49.0 dB against Engine A's 70.1 dB on the same 12 MP source.
//!
//! So this sweeps the same knobs against the metric that actually matters — PSNR
//! of the reconstructed HDR image versus the source — and reports speed beside
//! it, so the speed comparison in `docs/engine-comparison.md` can be made at
//! matched quality rather than at matched `--quality`.
//!
//! The PSNR definition is copied from `examples/roundtrip.rs` (peak = the
//! source's own maximum luma; pixels below 0.05 luma excluded) so the numbers are
//! directly comparable to the ones recorded there.
//!
//! Run: `cargo run --release --example probe_vt_quality -p tohdr-apple -- <hdr.tiff>`

use std::path::PathBuf;
use std::time::Instant;

use tohdr_apple::vtenc;
use tohdr_core::derive::DeriveOptions;
use tohdr_core::encode::{EncodeOptions, GainMapEncoder};
use tohdr_core::hdr::{derive_consistent, ToneMap};
use tohdr_core::{GainPlane, HdrRgb, Rgb};
use tohdr_heif::{CodedImage, MuxEngine, PlaneCodec};

/// The hardware codec with `RealTime` exposed, so the sweep can vary it.
struct TunedVt {
    realtime: bool,
}

fn coded(plane: vtenc::CodedPlane) -> CodedImage {
    CodedImage {
        width: plane.width,
        height: plane.height,
        bit_depth: 8,
        chroma: tohdr_heif::chroma_for(plane.monochrome),
        hvcc: plane.hvcc,
        data: plane.data,
    }
}

impl PlaneCodec for TunedVt {
    type Error = tohdr_apple::Error;

    fn name(&self) -> &'static str {
        "tuned-videotoolbox"
    }

    /// BT.709, matching `VideoToolboxCodec` — the whole point is to vary quality
    /// with everything else held at what the shipping codec does.
    fn base_colour(&self) -> tohdr_heif::ColourInfo {
        tohdr_heif::PlaneCodec::base_colour(&vtenc::VideoToolboxCodec)
    }

    fn encode_base(&self, base: &Rgb, quality: u8) -> Result<CodedImage, Self::Error> {
        Ok(coded(vtenc::encode_base_tuned(base, quality, self.realtime)?))
    }

    fn encode_gain(&self, gain: &GainPlane, quality: u8) -> Result<CodedImage, Self::Error> {
        Ok(coded(vtenc::encode_gain_tuned(gain, quality, self.realtime)?))
    }
}

/// PSNR of `got` against `src`, and the p99.9 relative error.
fn compare(src: &HdrRgb, got: &HdrRgb) -> Option<(f64, f64)> {
    if src.width != got.width || src.height != got.height {
        return None;
    }
    let mut sse = 0.0f64;
    let mut n = 0usize;
    let mut peak = 0.0f64;
    let mut rels: Vec<f64> = Vec::new();
    for y in 0..src.height {
        for x in 0..src.width {
            let a = src.luma(x, y) as f64;
            peak = peak.max(a);
            if a < 0.05 {
                continue;
            }
            let b = got.luma(x, y) as f64;
            rels.push(((b - a) / a).abs());
            sse += (b - a) * (b - a);
            n += 1;
        }
    }
    if n == 0 {
        return None;
    }
    rels.sort_by(|p, q| p.partial_cmp(q).unwrap());
    let p999 = rels[((rels.len() as f64 * 0.999) as usize).min(rels.len() - 1)];
    let mse = sse / n as f64;
    Some((20.0 * (peak / mse.sqrt()).log10(), p999))
}

fn main() {
    let src_path = PathBuf::from(
        std::env::args()
            .nth(1)
            .expect("usage: probe_vt_quality <hdr.tiff>"),
    );
    let hdr = tohdr_portable::load_hdr(&src_path).expect("load source");
    let white = hdr.peak_luma(0.001);
    let base = ToneMap::Reinhard { white }.to_sdr(&hdr);
    let (gain, meta) = derive_consistent(&hdr, &base, &DeriveOptions::default());
    println!(
        "source {}x{} ({:.2} MP), peak {white:.3}x\n",
        hdr.width,
        hdr.height,
        (hdr.width as f64 * hdr.height as f64) / 1e6
    );

    // Engine A at the default quality is the bar to clear.
    let opts = EncodeOptions::default();
    let t = Instant::now();
    let a = tohdr_apple::AppleEngine
        .encode(&base, &gain, &meta, &opts)
        .expect("engine A");
    let a_ms = t.elapsed().as_secs_f64() * 1000.0;
    let a_path = std::path::Path::new("out/vtq_apple.heic");
    std::fs::write(a_path, &a).expect("write");
    let (a_psnr, a_p999) = tohdr_apple::load_hdr(a_path)
        .ok()
        .and_then(|d| compare(&hdr, &d))
        .expect("decode engine A output");
    println!(
        "  Engine A          q{:<3}            {a_ms:8.1} ms  {:>10} B   PSNR {a_psnr:6.2} dB   p99.9 {:.2}%",
        opts.base_quality,
        a.len(),
        a_p999 * 100.0
    );
    println!();

    println!("  hardware, sweeping quality x RealTime:");
    for realtime in [true, false] {
        for quality in [85u8, 92, 95, 98, 100] {
            let engine = MuxEngine::new(TunedVt { realtime });
            let opts = EncodeOptions {
                base_quality: quality,
                gain_quality: quality,
                ..EncodeOptions::default()
            };
            let t = Instant::now();
            let bytes = match engine.encode(&base, &gain, &meta, &opts) {
                Ok(b) => b,
                Err(e) => {
                    println!("  realtime={realtime:<5} q{quality:<3}  encode failed: {e}");
                    continue;
                }
            };
            let ms = t.elapsed().as_secs_f64() * 1000.0;
            let path = std::path::Path::new("out/vtq_hw.heic");
            std::fs::write(path, &bytes).expect("write");
            let m = tohdr_apple::load_hdr(path).ok().and_then(|d| compare(&hdr, &d));
            match m {
                Some((psnr, p999)) => println!(
                    "  realtime={realtime:<5} q{quality:<3}         {ms:8.1} ms  {:>10} B   PSNR {psnr:6.2} dB   p99.9 {:.2}%   {:.2}x A{}",
                    bytes.len(),
                    p999 * 100.0,
                    ms / a_ms,
                    if psnr >= a_psnr { "  <- matches A's fidelity" } else { "" }
                ),
                None => println!("  realtime={realtime:<5} q{quality:<3}  decode failed"),
            }
        }
    }
}
