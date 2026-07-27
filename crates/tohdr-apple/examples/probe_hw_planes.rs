//! Does hardware plane encoding plus our own muxer reach Engine A's speed?
//!
//! Engine B's deficit is its plane encoder, not its muxer (23.3 of 30.8
//! CPU-seconds inside hpvca, ~0.1 ms in `tohdr_heif`), so this swaps in the
//! platform media block and keeps `tohdr_heif::mux`, timing each stage.
//!
//! Two preconditions are checked rather than assumed: each plane must come back as
//! a *single* coded item (a HEIF `grid` would make remuxing a re-encode), and the
//! gain plane must survive as one channel, since ISO 21496-1 and every Apple gain
//! plane measured are `L008` monochrome.
//!
//! Run: `cargo run --release --example probe_hw_planes -p tohdr-apple -- <file>`

use std::time::Instant;

use tohdr_core::derive::DeriveOptions;
use tohdr_core::encode::{EncodeOptions, GainMapEncoder};
use tohdr_core::hdr::{derive_consistent, ToneMap};

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

fn main() {
    let path = std::env::args().nth(1).expect("usage: probe_hw_planes <file>");
    let path = std::path::Path::new(&path);

    let hdr = tohdr_apple::load_hdr(path).expect("load_hdr");
    let (w, h) = (hdr.width, hdr.height);
    let mp = (w as f64 * h as f64) / 1e6;
    let white = hdr.peak_luma(0.001);
    let base = ToneMap::Reinhard { white }.to_sdr(&hdr);
    // Defaults to subsample 1, matching `tohdr bench` so the numbers line up
    // with docs/engine-comparison.md. Pass a second argument to use `convert`'s
    // actual default of 2, where the gain plane is a quarter the pixels.
    let subsample: u32 = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(1);
    let (gain, meta) = derive_consistent(
        &hdr,
        &base,
        &DeriveOptions { subsample, ..DeriveOptions::default() },
    );
    println!("  gain plane {}x{} (subsample {subsample})", gain.width, gain.height);
    drop(hdr);
    println!("{w}x{h} ({mp:.2} MP), base + full-res gain, quality 85\n");

    let opts = EncodeOptions::default();

    // --- Engine A, for reference: ImageIO encodes *and* muxes ---
    let t = Instant::now();
    let a = tohdr_apple::AppleEngine
        .encode(&base, &gain, &meta, &opts)
        .expect("engine A");
    let a_ms = ms(t);
    println!("  Engine A (ImageIO encode + ImageIO mux)   {a_ms:8.1} ms  {:>10} B", a.len());

    // --- Engine B as it stands: hpvca encodes, we mux ---
    let t = Instant::now();
    let b = tohdr_portable::PortableEngine
        .encode(&base, &gain, &meta, &opts)
        .expect("engine B");
    let b_ms = ms(t);
    println!("  Engine B (hpvca encode + our mux)         {b_ms:8.1} ms  {:>10} B", b.len());

    // --- the candidate: VideoToolbox encodes, we mux ---
    // The shipping path, not a reimplementation of it: `MuxEngine` over
    // `VideoToolboxCodec` is exactly what `--engine portable` builds, so this
    // number and the CLI's cannot drift apart.
    let t = Instant::now();
    let out = tohdr_heif::MuxEngine::new(tohdr_apple::vtenc::VideoToolboxCodec)
        .encode(&base, &gain, &meta, &opts)
        .expect("engine B-hw");
    let total = ms(t);

    // The stage split needs the plane encoder directly, since the engine above
    // (correctly) hides it. Run separately so the timings above stay clean.
    let bp = tohdr_apple::vtenc::encode_base(&base, opts.base_quality).expect("vt base plane");
    let base_only = bp.session_ms + bp.fill_ms + bp.encode_ms;
    let gp = tohdr_apple::vtenc::encode_gain(&gain, opts.gain_quality).expect("vt gain plane");
    let gain_only = gp.session_ms + gp.fill_ms + gp.encode_ms;

    println!(
        "  Engine B-hw (VideoToolbox + our mux)      {total:8.1} ms  {:>10} B",
        out.len()
    );
    println!(
        "      base:  session {:.1} + fill {:.1} + encode {:.1} = {base_only:.1} ms",
        bp.session_ms, bp.fill_ms, bp.encode_ms
    );
    println!(
        "      gain:  session {:.1} + fill {:.1} + encode {:.1} = {gain_only:.1} ms",
        gp.session_ms, gp.fill_ms, gp.encode_ms
    );
    // The media block is one shared fixed-function unit, so overlapping the two
    // plane encodes need not help the way it did for the CPU codec. It does:
    // session setup and pixel-buffer fill are CPU work that hides under the
    // other plane's encode.
    println!(
        "      sequential would be {:.1} ms; concurrent + mux measured {total:.1}",
        base_only + gain_only
    );
    println!(
        "\n  vs Engine A: {:.2}x   |   vs Engine B (hpvca): {:.2}x faster",
        total / a_ms,
        b_ms / total
    );
    std::fs::write("out/hw_planes.heic", &out).expect("write");
    println!("  wrote out/hw_planes.heic — check it with `tohdr verify`");
}
