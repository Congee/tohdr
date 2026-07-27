//! Does reusing a `VTCompressionSession` change the bytes, and what does it buy?
//!
//! Bytes first, speed second. `vtenc` guarantees identical pixels give identical
//! bytes, and a stateful encoder could make frame 2 of a session differ from frame
//! 1 of a fresh one -- which would make output depend on position in the batch, a
//! worse property than the milliseconds are worth.
//!
//! It is plausible they match: every frame is an IDR with
//! `MaxKeyFrameInterval = 1` and no reordering, and an IDR slice header carries no
//! `pic_order_cnt_lsb` (H.265 7.3.6.1), so position cannot leak in. Plausible is
//! not measured, hence this file.
//!
//! Run: `cargo run --release --example probe_vt_session_reuse -p tohdr-apple -- <hdr.tiff> [iters]`

use std::path::PathBuf;
use std::time::Instant;

use tohdr_apple::vtenc::{self, VideoToolboxCodec};
use tohdr_core::derive::DeriveOptions;
use tohdr_core::encode::{EncodeOptions, GainMapEncoder};
use tohdr_core::hdr::{derive_consistent, ToneMap};
use tohdr_heif::MuxEngine;

fn sha(bytes: &[u8]) -> String {
    // Not cryptographic and does not need to be: this only has to notice that
    // two multi-megabyte buffers differ.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// Where the first difference is, so a mismatch is diagnosable rather than just
/// reported.
fn first_diff(a: &[u8], b: &[u8]) -> Option<usize> {
    if a.len() != b.len() {
        return Some(a.len().min(b.len()));
    }
    a.iter().zip(b).position(|(x, y)| x != y)
}

fn main() {
    let mut args = std::env::args().skip(1);
    let src = PathBuf::from(
        args.next()
            .expect("usage: probe_vt_session_reuse <hdr.tiff> [iters]"),
    );
    let iters: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(5);

    let hdr = tohdr_portable::load_hdr(&src).expect("load source");
    let white = hdr.peak_luma(0.001);
    let base = ToneMap::Reinhard { white }.to_sdr(&hdr);
    let (gain, meta) = derive_consistent(&hdr, &base, &DeriveOptions::default());
    println!(
        "source {}x{} ({:.2} MP), gain {}x{}\n",
        base.width,
        base.height,
        (base.width as f64 * base.height as f64) / 1e6,
        gain.width,
        gain.height
    );
    drop(hdr);

    let q = 85u8;

    // --- 1. bytes ---------------------------------------------------------
    //
    // Each plane is checked on its own, because they are different sessions:
    // BGRA/Main for the base, L008/Monochrome for the gain.
    println!("== bytes: fresh session vs pooled session ==");
    for plane in ["base", "gain"] {
        let encode = |_: usize| -> (Vec<u8>, Vec<u8>, bool, f64) {
            if plane == "base" {
                let p = vtenc::encode_base(&base, q).expect("base encode");
                (p.data, p.hvcc, p.session_reused, p.session_ms)
            } else {
                let p = vtenc::encode_gain(&gain, q).expect("gain encode");
                (p.data, p.hvcc, p.session_reused, p.session_ms)
            }
        };

        // Two encodes with pooling off: both sessions are fresh. This is the
        // behaviour the recorded hashes in docs/engine-comparison.md were taken
        // under, so it is the reference.
        vtenc::set_session_reuse(false);
        let (fresh1, hvcc1, r1, _) = encode(0);
        let (fresh2, _, r2, _) = encode(1);
        assert!(!r1 && !r2, "pooling was supposed to be off");

        // Now with pooling on: the first is still a miss, the second a hit.
        vtenc::set_session_reuse(true);
        vtenc::drain_session_pool();
        let (miss, hvcc_miss, r3, ms_miss) = encode(2);
        let (hit, hvcc_hit, r4, ms_hit) = encode(3);
        assert!(!r3, "the first encode after a drain must be a miss");
        assert!(r4, "the second encode must have come from the pool");

        println!("  {plane}:");
        println!(
            "    fresh #1  {}  {:>10} B",
            sha(&fresh1),
            fresh1.len()
        );
        println!(
            "    fresh #2  {}  {:>10} B   {}",
            sha(&fresh2),
            fresh2.len(),
            if fresh2 == fresh1 { "same" } else { "DIFFERS" }
        );
        println!(
            "    pool miss {}  {:>10} B   {}   session {ms_miss:6.1} ms",
            sha(&miss),
            miss.len(),
            if miss == fresh1 { "same" } else { "DIFFERS" }
        );
        println!(
            "    pool hit  {}  {:>10} B   {}   session {ms_hit:6.1} ms",
            sha(&hit),
            hit.len(),
            if hit == fresh1 { "same" } else { "DIFFERS" }
        );
        if hit != fresh1 {
            println!(
                "    !! first difference at byte {:?} of {} — reuse is NOT byte-transparent",
                first_diff(&fresh1, &hit),
                fresh1.len()
            );
        }
        // The `hvcC` describes the bitstream; a pooled session that reported a
        // stale one would produce a file a decoder reads wrongly, which is
        // harder to notice than differing slice bytes.
        let hvcc_same = hvcc_miss == hvcc1 && hvcc_hit == hvcc1;
        println!(
            "    hvcC      {}  {:>10} B   {}",
            sha(&hvcc1),
            hvcc1.len(),
            if hvcc_same { "same on all four" } else { "DIFFERS" }
        );
    }
    println!();

    // --- 2. time ----------------------------------------------------------
    //
    // Through `MuxEngine`, not the raw plane calls, because that is what a
    // conversion actually does — and because the gain plane runs on a scoped
    // thread that does not outlive the call, which is exactly the case a
    // per-thread cache would fail to serve and a process-wide pool does.
    println!("== time: {iters} full encodes of the same pair ==");
    let opts = EncodeOptions {
        base_quality: q,
        gain_quality: q,
        ..EncodeOptions::default()
    };
    for reuse in [false, true] {
        vtenc::set_session_reuse(reuse);
        vtenc::drain_session_pool();
        vtenc::set_session_reuse(reuse);
        let engine = MuxEngine::new(VideoToolboxCodec);
        let mut times = Vec::new();
        let mut hashes = Vec::new();
        for _ in 0..iters {
            let t = Instant::now();
            let bytes = engine.encode(&base, &gain, &meta, &opts).expect("encode");
            times.push(t.elapsed().as_secs_f64() * 1000.0);
            hashes.push(sha(&bytes));
        }
        let first = times[0];
        let rest: f64 = if times.len() > 1 {
            times[1..].iter().sum::<f64>() / (times.len() - 1) as f64
        } else {
            f64::NAN
        };
        let all_same = hashes.iter().all(|h| *h == hashes[0]);
        println!(
            "  reuse={:<5}  first {first:7.1} ms   later mean {rest:7.1} ms   {}   output {}",
            reuse,
            hashes[0],
            if all_same {
                "identical across iterations"
            } else {
                "OUTPUT VARIES ACROSS ITERATIONS"
            }
        );
        let (hits, misses) = vtenc::session_pool_stats();
        println!("    pool so far: {hits} hit(s), {misses} miss(es)");
    }

    // --- 3. where the saving comes from -----------------------------------
    //
    // Creating the session is only part of it. VideoToolbox brings the encoder
    // up lazily, so the *first frame* on a session pays initialisation that
    // `session_ms` does not see — which is why the measured saving is larger
    // than `session_ms` alone predicts. Splitting the stages is the only way to
    // tell those two apart.
    println!("\n== stage breakdown, {iters} sequential base encodes, pooling on ==");
    vtenc::set_session_reuse(true);
    vtenc::drain_session_pool();
    println!("            session      fill    encode     total   from pool");
    for i in 0..iters {
        let p = vtenc::encode_base(&base, q).expect("base encode");
        println!(
            "  #{i:<2}      {:7.1}   {:7.1}   {:7.1}   {:7.1}     {}",
            p.session_ms,
            p.fill_ms,
            p.encode_ms,
            p.session_ms + p.fill_ms + p.encode_ms,
            p.session_reused
        );
    }

    // Leave the process as the library defaults, so a later probe in the same
    // binary is not silently measuring a disabled pool.
    vtenc::set_session_reuse(true);
}
