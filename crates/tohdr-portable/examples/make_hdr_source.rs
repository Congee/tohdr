//! Generate a deterministic HDR test image as an uncompressed TIFF.
//!
//! ```text
//! cargo run --release -p tohdr-portable --example make_hdr_source -- \
//!     out.tiff [--width 1024] [--height 768] [--peak 8.0] \
//!     [--format f32|u16] [--u16-white 1.0]
//! ```
//!
//! A fair engine benchmark needs the same input bytes on both sides, and an
//! end-to-end gain-map test needs a source whose true above-white content is
//! known exactly rather than inferred from a photograph.
//!
//! Two sample formats, because the two engines reach for different decoders:
//! `f32` is 32-bit IEEE float, linear, 1.0 == SDR diffuse white, and is the
//! reference. `u16` is 16-bit unsigned scaled so `--u16-white` maps to 65535 --
//! lossy above white and needing the scale communicated out of band, which is
//! exactly the ambiguity an unmanaged 16-bit TIFF has in the wild, so that path
//! is testable too.
//!
//! Byte-identical to the `tools/make_hdr_source.py` it replaces, so the
//! benchmark numbers in `docs/performance.md` still describe this input. That is
//! why the arithmetic is spelled out in f64 in Python's evaluation order and the
//! u16 path rounds half-to-even rather than half-away-from-zero.

use std::f64::consts::PI;

// TIFF tags we emit.
const IMAGE_WIDTH: u16 = 256;
const IMAGE_LENGTH: u16 = 257;
const BITS_PER_SAMPLE: u16 = 258;
const COMPRESSION: u16 = 259;
const PHOTOMETRIC: u16 = 262;
const STRIP_OFFSETS: u16 = 273;
const SAMPLES_PER_PIXEL: u16 = 277;
const ROWS_PER_STRIP: u16 = 278;
const STRIP_BYTE_COUNTS: u16 = 279;
const PLANAR_CONFIG: u16 = 284;
const SAMPLE_FORMAT: u16 = 339;

const TYPE_SHORT: u16 = 3;
const TYPE_LONG: u16 = 4;

/// Deterministic linear-light RGB, 1.0 == SDR white.
///
/// Content is chosen to exercise the things a gain map is bad at: a broad SDR
/// gradient (the part that must survive tone mapping intact), specular discs far
/// above white (the headroom the map has to carry), a saturated red highlight
/// clipping ONE channel, which a luma-derived single-channel gain map necessarily
/// under-corrects, and near-black patches, where the base/alt offsets decide the
/// ratio.
fn scene(width: u32, height: u32, peak: f64) -> Vec<f64> {
    let mut px = vec![0.0f64; width as usize * height as usize * 3];
    // Three specular discs at increasing intensity: centre, radius, strength.
    const DISCS: [(f64, f64, f64, f64); 3] =
        [(0.25, 0.70, 0.070, 0.30), (0.50, 0.70, 0.055, 0.65), (0.75, 0.70, 0.040, 1.00)];

    for y in 0..height {
        for x in 0..width {
            let u = (f64::from(x) + 0.5) / f64::from(width);
            let v = (f64::from(y) + 0.5) / f64::from(height);

            // SDR base gradient: a gentle diagonal, comfortably below white.
            let mut r = 0.15 + 0.45 * u;
            let mut g = 0.18 + 0.40 * v;
            let mut b = 0.22 + 0.30 * (1.0 - u);

            // Near-black corner patch.
            if u < 0.12 && v < 0.12 {
                r = 0.0015;
                g = 0.0015;
                b = 0.0015;
            }

            for (dx, dy, rad, mult) in DISCS {
                let d = (u - dx).hypot(v - dy) / rad;
                if d < 1.0 {
                    // Smooth falloff so the map has a gradient to encode, not a
                    // step edge that only tests clamping.
                    let f = (d * PI / 2.0).cos();
                    let lift = 1.0 + (peak - 1.0) * mult * (f * f);
                    r *= lift;
                    g *= lift;
                    b *= lift;
                }
            }

            // Saturated red highlight: red far above white, green/blue low.
            let d = (u - 0.5).hypot(v - 0.25) / 0.09;
            if d < 1.0 {
                let f = (d * PI / 2.0).cos();
                r = r.max(0.2 + (peak * 0.9) * (f * f));
                g = g.min(0.10);
                b = b.min(0.06);
            }

            let i = (y as usize * width as usize + x as usize) * 3;
            px[i] = r;
            px[i + 1] = g;
            px[i + 2] = b;
        }
    }

    // Pin the exact peak so tests can assert on it.
    px[0] = peak;
    px[1] = peak;
    px[2] = peak;
    px
}

/// Uncompressed, single-strip, contiguous RGB TIFF, little-endian.
fn write_tiff(path: &str, width: u32, height: u32, px: &[f64], format: &str, u16_white: f64)
    -> std::io::Result<usize>
{
    let (bits, sfmt, body) = if format == "f32" {
        let mut body = Vec::with_capacity(px.len() * 4);
        for &v in px {
            body.extend_from_slice(&(v as f32).to_le_bytes());
        }
        (32u16, 3u16, body) // IEEE float
    } else {
        let scale = 65535.0 / u16_white.max(1e-6);
        let mut body = Vec::with_capacity(px.len() * 2);
        for &v in px {
            // Python's `round()` is half-to-even, and this has to match it.
            let q = (v * scale).round_ties_even().clamp(0.0, 65535.0) as u16;
            body.extend_from_slice(&q.to_le_bytes());
        }
        (16u16, 1u16, body) // unsigned integer
    };

    // Tags in ascending order, which TIFF requires. `None` is patched below.
    let entries: [(u16, u16, u32, Option<u32>); 11] = [
        (IMAGE_WIDTH, TYPE_LONG, 1, Some(width)),
        (IMAGE_LENGTH, TYPE_LONG, 1, Some(height)),
        (BITS_PER_SAMPLE, TYPE_SHORT, 3, None),
        (COMPRESSION, TYPE_SHORT, 1, Some(1)), // none
        (PHOTOMETRIC, TYPE_SHORT, 1, Some(2)), // RGB
        (STRIP_OFFSETS, TYPE_LONG, 1, None),
        (SAMPLES_PER_PIXEL, TYPE_SHORT, 1, Some(3)),
        (ROWS_PER_STRIP, TYPE_LONG, 1, Some(height)),
        (STRIP_BYTE_COUNTS, TYPE_LONG, 1, Some(body.len() as u32)),
        (PLANAR_CONFIG, TYPE_SHORT, 1, Some(1)), // chunky
        (SAMPLE_FORMAT, TYPE_SHORT, 3, None),
    ];

    let header_len = 8usize;
    let ifd_len = 2 + 12 * entries.len() + 4;
    // Out-of-line values: BitsPerSample[3] and SampleFormat[3], 6 bytes each.
    let extra_off = header_len + ifd_len;
    let bps_off = extra_off as u32;
    let sfmt_off = (extra_off + 6) as u32;
    let strip_off = (extra_off + 12) as u32;

    let mut out: Vec<u8> = Vec::with_capacity(strip_off as usize + body.len());
    out.extend_from_slice(b"II");
    out.extend_from_slice(&42u16.to_le_bytes());
    out.extend_from_slice(&(header_len as u32).to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    for (tag, typ, count, value) in entries {
        out.extend_from_slice(&tag.to_le_bytes());
        out.extend_from_slice(&typ.to_le_bytes());
        out.extend_from_slice(&count.to_le_bytes());
        let payload = match tag {
            BITS_PER_SAMPLE => bps_off,
            SAMPLE_FORMAT => sfmt_off,
            STRIP_OFFSETS => strip_off,
            // A SHORT value sits in the low 2 bytes of the 4-byte field.
            _ => value.expect("only the out-of-line tags carry no value"),
        };
        out.extend_from_slice(&payload.to_le_bytes());
    }
    out.extend_from_slice(&0u32.to_le_bytes()); // no next IFD
    assert_eq!(out.len(), extra_off);
    for _ in 0..3 {
        out.extend_from_slice(&bits.to_le_bytes());
    }
    for _ in 0..3 {
        out.extend_from_slice(&sfmt.to_le_bytes());
    }
    assert_eq!(out.len(), strip_off as usize);
    out.extend_from_slice(&body);

    std::fs::write(path, &out)?;
    Ok(out.len())
}

/// Python's `{:,}`, so the output lines match the tool this replaces.
fn commas(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.char_indices() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn main() -> std::io::Result<()> {
    let usage = "usage: make_hdr_source <out.tiff> [--width N] [--height N] \
                 [--peak F] [--format f32|u16] [--u16-white F]";
    let (mut path, mut width, mut height) = (None, 1024u32, 768u32);
    let (mut peak, mut format, mut u16_white) = (8.0f64, "f32".to_string(), 1.0f64);
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut next = || args.next().unwrap_or_else(|| panic!("{a} needs a value"));
        match a.as_str() {
            "--width" => width = next().parse().expect("--width"),
            "--height" => height = next().parse().expect("--height"),
            "--peak" => peak = next().parse().expect("--peak"),
            "--u16-white" => u16_white = next().parse().expect("--u16-white"),
            "--format" => {
                format = next();
                assert!(format == "f32" || format == "u16", "--format must be f32 or u16");
            }
            "-h" | "--help" => {
                println!("{usage}");
                return Ok(());
            }
            other if other.starts_with('-') => panic!("unknown flag {other}\n{usage}"),
            other => path = Some(other.to_string()),
        }
    }
    let path = path.expect(usage);

    let px = scene(width, height, peak);
    let n = write_tiff(&path, width, height, &px, &format, u16_white)?;

    let above = px
        .chunks_exact(3)
        .filter(|c| 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2] > 1.0)
        .count();
    let total = width as usize * height as usize;
    println!("{path}: {width}x{height} {format}, {} bytes", commas(n));
    println!("  declared peak {peak:?}x SDR white = {:.4} stops", peak.log2());
    println!(
        "  {} of {} pixels ({:.2}%) above SDR white",
        commas(above),
        commas(total),
        100.0 * above as f64 / total as f64
    );
    if format == "u16" {
        let fits = if u16_white < peak { "lossy for this peak" } else { "peak fits" };
        println!("  NOTE: u16 clips above {u16_white:?}x; {fits}");
    }
    Ok(())
}
