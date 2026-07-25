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

/// *Current* resident size in bytes, via `task_info(MACH_TASK_BASIC_INFO)`.
///
/// [`peak_rss`] is a high-water mark and so can only ever rise; it cannot show
/// whether freeing a buffer actually returned its pages to the OS. That is the
/// question that decides whether dropping a dead allocation before a later
/// allocating phase lowers the peak or merely hands the pages back to the
/// allocator's cache, so it needs a gauge that can fall.
fn live_rss() -> u64 {
    // mach_task_basic_info: 3 x u64, 2 x time_value_t (2 x i32), policy_t,
    // suspend_count — 48 bytes, i.e. 12 natural_t units, which is what the
    // MACH_TASK_BASIC_INFO_COUNT macro expands to.
    #[repr(C)]
    #[derive(Default)]
    struct MachTaskBasicInfo {
        virtual_size: u64,
        resident_size: u64,
        resident_size_max: u64,
        user_time: [i32; 2],
        system_time: [i32; 2],
        policy: i32,
        suspend_count: i32,
    }
    const MACH_TASK_BASIC_INFO: i32 = 20;
    unsafe extern "C" {
        fn mach_task_self() -> u32;
        fn task_info(task: u32, flavor: i32, info: *mut MachTaskBasicInfo, count: *mut u32) -> i32;
    }
    let mut info = MachTaskBasicInfo::default();
    let mut count = 12u32;
    if unsafe { task_info(mach_task_self(), MACH_TASK_BASIC_INFO, &mut info, &mut count) } == 0 {
        info.resident_size
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
    live_after: u64,
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
                live_after: live_rss(),
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

    // `hdr` is dead here, and it is the largest live buffer in the process.
    // Dropping it before the allocating encode phase is only worth doing if the
    // pages actually go back to the OS, so this phase exists to measure that:
    // watch `live RSS`, not `peak RSS`, which by construction cannot fall.
    let (hdr_w, hdr_h) = (hdr.width, hdr.height);
    phase!("drop HdrRgb", drop(hdr));

    // Dropping it is not enough: macOS libmalloc keeps the freed span in its
    // own cache, so live RSS does not move and the later encode allocates on
    // top of a buffer nothing can read any more. This asks the allocator to
    // hand cached pages back to the kernel.
    phase!("malloc pressure relief", {
        unsafe extern "C" {
            fn malloc_zone_pressure_relief(zone: *mut core::ffi::c_void, goal: usize) -> usize;
        }
        unsafe { malloc_zone_pressure_relief(core::ptr::null_mut(), 0) }
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
        hdr_w,
        hdr_h
    );
    println!(
        "  buffers: HdrRgb {:.0} MiB, base Rgb {:.0} MiB, gain {:.1} MiB, output {:.1} MiB",
        mib(hdr_bytes),
        mib(base_bytes),
        mib(gain.data.len() as u64),
        mib(bytes.len() as u64),
    );
    println!("  headroom {:.3} stops\n", meta.alt_headroom);

    println!(
        "  {:<28} {:>9}  {:>6}  {:>10}  {:>10}",
        "phase", "ms", "%", "peak RSS", "live RSS"
    );
    for p in &phases {
        println!(
            "  {:<28} {:>9.1}  {:>5.1}%  {:>8.0} MiB  {:>8.0} MiB",
            p.name,
            p.ms,
            100.0 * p.ms / total,
            mib(p.rss_after),
            mib(p.live_after)
        );
    }
    println!("  {:<28} {:>9.1}  {:>6}", "TOTAL", total, "");
    println!(
        "\n  throughput: {:.1} MP/s overall, {:.1} MP/s excluding decode",
        mp / (total / 1000.0),
        mp / ((total - phases[0].ms) / 1000.0)
    );
}
