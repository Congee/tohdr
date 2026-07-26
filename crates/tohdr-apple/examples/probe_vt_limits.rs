//! Where does the media block stop accepting a still, and does the `--max-size`
//! search make it worse?
//!
//! A Lightroom export of a 60 MP Sony ARW through `--engine portable` with a size
//! budget failed with
//!
//! ```text
//!   tohdr: error: encoding within budget: hardware-videotoolbox:
//!   VideoToolbox encode callback reported -17691
//! ```
//!
//! asynchronously — `EncodeFrame` and `CompleteFrames` both returned 0, so nothing
//! at the call site refused the job.
//!
//! # What this measured
//!
//! Not geometry. A single frame of any realistic size encodes fine, up to a
//! deliberately absurd 103.8 MP. What fails is the *number of sessions alive at
//! once*, and idle pooled sessions count exactly as much as in-flight ones:
//!
//! ```text
//!   9504x6336 (60.2 MP)    3 live ok, 4th fails      4 concurrent: 1 of 4 failed
//!   8064x6048 (48.8 MP)    5 live ok, 6th fails      6 concurrent: 3 of 6 failed
//!   4032x3024 (12.2 MP)   15 live ok (probe's cap)
//! ```
//!
//! The `--max-size` search reached it because every quality it tries is a distinct
//! `SessionKey`: at 60 MP it failed on the *fourth* of seven attempts, having left
//! three 60 MP sessions idle in the pool. That is the export the user saw.
//!
//! The concurrency column is why the fix is a gate on live sessions
//! ([`MAX_LIVE_PIXELS`](../src/vtenc.rs)) rather than a smaller pool: bounding
//! only the pool would have left `tohdr batch --jobs 4` broken at 60 MP with an
//! empty pool.
//!
//! # What it does now
//!
//! Re-run it as a regression check — with the gate in place every section should
//! report zero failures, and the byte counts should be unchanged (the gate decides
//! *when* a session exists, never how it is configured). The pre-fix numbers above
//! are what this printed before; there is no switch to turn the gate off, so
//! reproducing them means reverting it.
//!
//! Run: `cargo run --release --example probe_vt_limits -p tohdr-apple`

use tohdr_apple::vtenc;
use tohdr_core::{GainPlane, Rgb};

/// A cheap deterministic pattern. Not flat: a constant frame can encode
/// successfully for uninteresting reasons, and costs the encoder nothing.
fn synth_base(width: u32, height: u32) -> Rgb {
    let (w, h) = (width as usize, height as usize);
    let mut data = vec![0u16; w * h * 3];
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) * 3;
            data[i] = ((x * 7 + y * 3) % 256) as u16;
            data[i + 1] = ((x ^ y) % 256) as u16;
            data[i + 2] = ((x / 3 + y * 5) % 256) as u16;
        }
    }
    Rgb { width, height, bits: 8, data }
}

fn synth_gain(width: u32, height: u32) -> GainPlane {
    let n = width as usize * height as usize;
    GainPlane {
        width,
        height,
        data: (0..n).map(|i| (i % 251) as u8).collect(),
    }
}

fn report(label: &str, r: Result<vtenc::CodedPlane, tohdr_apple::Error>) -> bool {
    match r {
        Ok(p) => {
            println!(
                "  {label:<28} ok    {:>11} B   ({:.0} ms encode)",
                p.data.len(),
                p.encode_ms
            );
            true
        }
        Err(e) => {
            println!("  {label:<28} FAIL  {e}");
            false
        }
    }
}

fn mp(w: u32, h: u32) -> f64 {
    (w as f64 * h as f64) / 1e6
}

fn main() {
    let mut failures = 0usize;

    // --- 1. geometry, one fresh session each -------------------------------
    //
    // Drained between entries so each is a cold encode: if a large frame fails
    // here, nothing else was holding the encoder.
    println!("geometry (pool drained between each)");
    for (w, h) in [
        (4032u32, 3024u32), // 12 MP
        (8064, 6048),       // 48 MP, an iPhone 17 Pro capture
        (8192, 6144),       // 50 MP
        (9504, 6336),       // 60 MP, the Sony A7R V render Lightroom hands us
        (9600, 6376),       // the ARW's full sensor readout
        (16384, 6336),      // deliberately absurd, to prove size is not the limit
    ] {
        vtenc::drain_session_pool();
        let base = synth_base(w, h);
        if !report(
            &format!("{w}x{h} colour ({:.1} MP)", mp(w, h)),
            vtenc::encode_base(&base, 85),
        ) {
            failures += 1;
        }
    }

    // Gain planes are monochrome and go through a different profile, so a limit
    // on one is not evidence about the other.
    println!("\ngain plane, monochrome (pool drained between each)");
    for (w, h) in [(4752u32, 3168u32), (9504, 6336)] {
        vtenc::drain_session_pool();
        let gain = synth_gain(w, h);
        if !report(&format!("{w}x{h} mono"), vtenc::encode_gain(&gain, 85)) {
            failures += 1;
        }
    }

    // --- 2. the quality sequence the budget search actually runs -----------
    //
    // encode_within_budget tries full quality, then the floor, then bisects.
    // Every quality is a distinct SessionKey, so none of these hit the pool and
    // all of them would stay in it. This is the export that failed, and it failed
    // on attempt 4.
    let (w, h) = (9504u32, 6336u32);
    println!("\nbudget search's quality sequence at {w}x{h}, pool left alone");
    vtenc::drain_session_pool();
    let base = synth_base(w, h);
    let gain = synth_gain(w / 2, h / 2);
    let mut broke_at = None;
    for (i, q) in [85u8, 40, 62, 73, 79, 82, 84].iter().enumerate() {
        let ok_base = report(
            &format!("attempt {} base q{q}", i + 1),
            vtenc::encode_base(&base, *q),
        );
        let ok_gain = report(
            &format!("attempt {} gain q{q}", i + 1),
            vtenc::encode_gain(&gain, *q),
        );
        println!(
            "      {:.0} MP live, pool: {} hits, {} misses",
            vtenc::live_session_pixels() as f64 / 1e6,
            vtenc::session_pool_stats().0,
            vtenc::session_pool_stats().1
        );
        if !ok_base || !ok_gain {
            failures += 1;
            broke_at = broke_at.or(Some(i + 1));
        }
    }
    match broke_at {
        Some(n) => println!("\nfirst failure at attempt {n} -- sessions accumulated"),
        None => println!("\nthe whole sequence encoded"),
    }

    // --- 3. many sessions of one geometry ----------------------------------
    //
    // Pre-fix this found the ceiling, because every quality left a session behind.
    // With the gate it finds nothing: the totals below are what was *asked for*,
    // and the live figure alongside is what was allowed to exist at the end.
    println!("\n15 distinct qualities per geometry, nothing drained");
    for (gw, gh) in [(9504u32, 6336u32), (8064, 6048), (4032, 3024), (2016, 1512)] {
        vtenc::drain_session_pool();
        let b = synth_base(gw, gh);
        let mut n = 0;
        let mut failed = 0;
        for q in (1..=15).map(|i| 100 - i as u8) {
            match vtenc::encode_base(&b, q) {
                Ok(_) => n += 1,
                Err(_) => failed += 1,
            }
        }
        failures += failed;
        println!(
            "  {gw}x{gh} ({:>5.1} MP)  {n:>2} ok, {failed} failed   \
             ({:>6.1} MP requested, {:>5.1} MP live at the end)",
            mp(gw, gh),
            15.0 * mp(gw, gh),
            vtenc::live_session_pixels() as f64 / 1e6,
        );
    }

    // --- 4. the same sequence with idle sessions released by hand ----------
    //
    // The crude form of the fix, kept because it isolates the cause: if this
    // passes while section 2 fails, idle sessions holding media-block resources
    // is the whole story.
    println!("\nsame sequence at {w}x{h}, pool drained between attempts");
    vtenc::drain_session_pool();
    for (i, q) in [85u8, 40, 62, 73, 79, 82, 84].iter().enumerate() {
        vtenc::drain_session_pool();
        if !report(
            &format!("attempt {} base q{q}", i + 1),
            vtenc::encode_base(&base, *q),
        ) {
            failures += 1;
        }
        if !report(
            &format!("attempt {} gain q{q}", i + 1),
            vtenc::encode_gain(&gain, *q),
        ) {
            failures += 1;
        }
    }

    // --- 5. concurrency, which the pool has nothing to do with -------------
    //
    // `tohdr batch --jobs N` reaches the same ceiling from the other side. Pre-fix
    // 4 concurrent 60 MP encodes lost one and 6 lost three, with an empty pool
    // throughout -- so the gate has to bound live sessions, not pooled ones.
    println!("\nconcurrent encodes at {w}x{h}, empty pool");
    for jobs in [2usize, 3, 4, 6] {
        vtenc::drain_session_pool();
        let base = &base;
        // Distinct qualities so no thread can be served from another's session,
        // which is also what `batch` does when its files differ in quality.
        let failed = std::thread::scope(|s| {
            let handles: Vec<_> = (0..jobs)
                .map(|i| s.spawn(move || vtenc::encode_base(base, 100 - i as u8)))
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().expect("encode thread"))
                .filter(|r| r.is_err())
                .count()
        });
        failures += failed;
        println!(
            "  {jobs} concurrent ({:>6.1} MP requested)   {failed} failed",
            jobs as f64 * mp(w, h)
        );
    }

    vtenc::drain_session_pool();
    println!(
        "\n{}",
        if failures == 0 {
            "no failures: the gate holds".to_string()
        } else {
            format!("{failures} failing encode(s)")
        }
    );
    std::process::exit(if failures == 0 { 0 } else { 1 });
}
