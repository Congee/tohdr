//! `cargo run --release --example profile -p tohdr-apple -- <file>`
//!
//! Phase-by-phase wall clock and peak resident memory for the full convert
//! pipeline, so optimization targets the phase that actually costs, rather
//! than the one that looks slow.

use std::time::Instant;

use tohdr_core::derive::DeriveOptions;
use tohdr_core::encode::{EncodeOptions, GainMapEncoder};
use tohdr_core::hdr::{derive_consistent, ToneMap};

/// Peak resident set size in bytes, via `getrusage`. macOS reports `ru_maxrss`
/// in bytes (Linux uses kilobytes); this only ever runs on macOS.
fn peak_rss() -> u64 {
    #[repr(C)]
    #[derive(Default)]
    struct Rusage {
        ru_utime: [i64; 2],
        ru_stime: [i64; 2],
        ru_maxrss: i64,
        rest: [i64; 14],
    }
    unsafe extern "C" {
        fn getrusage(who: i32, usage: *mut Rusage) -> i32;
    }
    let mut u = Rusage::default();
    if unsafe { getrusage(0, &mut u) } == 0 {
        u.ru_maxrss as u64
    } else {
        0
    }
}

fn mib(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

struct Phase {
    name: &'static str,
    ms: f64,
    rss_after: u64,
}

fn main() {
    let path = std::env::args().nth(1).expect("usage: profile <file>");
    let path = std::path::Path::new(&path);
    let mut phases: Vec<Phase> = Vec::new();

    macro_rules! phase {
        ($name:expr, $body:expr) => {{
            let t = Instant::now();
            let out = $body;
            phases.push(Phase {
                name: $name,
                ms: t.elapsed().as_secs_f64() * 1000.0,
                rss_after: peak_rss(),
            });
            out
        }};
    }

    let hdr = phase!("decode (ImageIO -> HdrRgb)", {
        tohdr_apple::load_hdr(path).expect("load_hdr")
    });
    let mp = (hdr.width as f64 * hdr.height as f64) / 1.0e6;
    let hdr_bytes = hdr.data.len() as u64 * 4;

    let white = phase!("peak_luma", hdr.peak_luma(0.001));
    let base = phase!("tone map -> SDR base", {
        ToneMap::Reinhard { white }.to_sdr(&hdr)
    });
    let base_bytes = base.data.len() as u64 * 2;

    let (gain, meta) = phase!("derive gain plane", {
        derive_consistent(&hdr, &base, &DeriveOptions::default())
    });

    let opts = EncodeOptions::default();
    let bytes = phase!("encode (Engine A)", {
        tohdr_apple::AppleEngine
            .encode(&base, &gain, &meta, &opts)
            .expect("encode")
    });

    let total: f64 = phases.iter().map(|p| p.ms).sum();

    println!(
        "{}  {}x{}  {mp:.1} MP",
        path.display(),
        hdr.width,
        hdr.height
    );
    println!(
        "  buffers: HdrRgb {:.0} MiB, base Rgb {:.0} MiB, gain {:.1} MiB, output {:.1} MiB",
        mib(hdr_bytes),
        mib(base_bytes),
        mib(gain.data.len() as u64),
        mib(bytes.len() as u64),
    );
    println!("  headroom {:.3} stops\n", meta.alt_headroom);

    println!("  {:<28} {:>9}  {:>6}  {:>10}", "phase", "ms", "%", "peak RSS");
    for p in &phases {
        println!(
            "  {:<28} {:>9.1}  {:>5.1}%  {:>8.0} MiB",
            p.name,
            p.ms,
            100.0 * p.ms / total,
            mib(p.rss_after)
        );
    }
    println!("  {:<28} {:>9.1}  {:>6}", "TOTAL", total, "");
    println!(
        "\n  throughput: {:.1} MP/s overall, {:.1} MP/s excluding decode",
        mp / (total / 1000.0),
        mp / ((total - phases[0].ms) / 1000.0)
    );
}
