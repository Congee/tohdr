//! What does it cost to make Engine B-hw match Engine A's *bitrate*, and how
//! fast is it there?
//!
//! `probe_hw_planes` showed Engine B-hw at 1.38x Engine A while emitting 17%
//! more bytes at the same nominal quality — so part of that gap is simply more
//! bits being coded, not a slower encoder. VideoToolbox's `Quality` and
//! ImageIO's `LossyCompressionQuality` are not the same scale, and nothing
//! requires 0.85 to mean the same thing to both.
//!
//! This sweeps quality and the `RealTime` hint for the base plane, so speed can
//! be compared at equal output size rather than at equal nominal quality.
//!
//! Run: `cargo run --release --example probe_vt_tuning -p tohdr-apple -- <file>`

use std::time::Instant;

use tohdr_core::derive::DeriveOptions;
use tohdr_core::encode::{EncodeOptions, GainMapEncoder};
use tohdr_core::hdr::{derive_consistent, ToneMap};

fn main() {
    let path = std::env::args().nth(1).expect("usage: probe_vt_tuning <file>");
    let path = std::path::Path::new(&path);
    let hdr = tohdr_apple::load_hdr(path).expect("load_hdr");
    let white = hdr.peak_luma(0.001);
    let base = ToneMap::Reinhard { white }.to_sdr(&hdr);
    let (gain, meta) = derive_consistent(
        &hdr,
        &base,
        &DeriveOptions { subsample: 2, ..DeriveOptions::default() },
    );
    drop(hdr);
    let opts = EncodeOptions::default();

    let t = Instant::now();
    let a = tohdr_apple::AppleEngine.encode(&base, &gain, &meta, &opts).expect("A");
    let a_ms = t.elapsed().as_secs_f64() * 1000.0;
    println!(
        "Engine A: {a_ms:.1} ms, {} B total (base+gain+container)\n",
        a.len()
    );

    println!("  {:<26} {:>9}  {:>11}", "base plane config", "ms", "bytes");
    for rt in [false, true] {
        for q in [85u8, 75, 65, 55] {
            let p = tohdr_apple::vtenc::encode_base_tuned(&base, q, rt).expect("base");
            println!(
                "  realtime={rt:<5} q{q:<3}            {:>9.1}  {:>11}   (session {:.1} fill {:.1} enc {:.1})",
                p.session_ms + p.fill_ms + p.encode_ms,
                p.data.len(),
                p.session_ms,
                p.fill_ms,
                p.encode_ms
            );
        }
    }
    println!("\n  {:<26} {:>9}  {:>11}", "gain plane config", "ms", "bytes");
    for rt in [false, true] {
        let p = tohdr_apple::vtenc::encode_gain_tuned(&gain, 85, rt).expect("gain");
        println!(
            "  realtime={rt:<5} q85            {:>9.1}  {:>11}",
            p.session_ms + p.fill_ms + p.encode_ms,
            p.data.len()
        );
    }
}
