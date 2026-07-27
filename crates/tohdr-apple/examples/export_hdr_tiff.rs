//! Turn anything ImageIO can decode into the `<hdr.tiff>` every probe here asks
//! for.
//!
//! The measurements in `docs/` are taken on TIFF fixtures, because the probes
//! decode with `tohdr_portable` on purpose -- either engine's own decoder would
//! privilege its own reading. Nothing in the tree could *produce* one, so a
//! deleted fixture took its table row with it.
//!
//! Float32 RGB, linear, extended range: the one flavour `load_hdr` reads back as
//! linear light rather than PQ or SDR sRGB, so `load_hdr(export(x)) == x`.
//!
//! Run: `cargo run --release --example export_hdr_tiff -p tohdr-apple -- <src> <out.tiff>`

use std::io::Write;
use std::path::PathBuf;

fn main() {
    let mut args = std::env::args().skip(1);
    let src = PathBuf::from(
        args.next()
            .expect("usage: export_hdr_tiff <src> <out.tiff>"),
    );
    let dst = PathBuf::from(args.next().expect("usage: export_hdr_tiff <src> <out.tiff>"));

    // ImageIO, because that is the decoder that reads camera RAW — the whole
    // point is to get a photographic source into a form the portable path can
    // read.
    let hdr = tohdr_apple::load_hdr(&src).expect("decode source");
    let (w, h) = (hdr.width, hdr.height);
    println!(
        "{} -> {}x{} ({:.2} MP), peak luma {:.3}x",
        src.display(),
        w,
        h,
        (w as f64 * h as f64) / 1e6,
        hdr.peak_luma(0.001)
    );

    // A minimal little-endian baseline TIFF: one strip, 3 samples of IEEE float.
    // Hand-written rather than pulled from a crate because `tohdr-apple` does not
    // depend on `image` and this is 12 tags — adding a dependency to an example
    // would put it in the crate's lockfile for everyone.
    let bytes_per_px = 12u32;
    let data_len = w as u64 * h as u64 * bytes_per_px as u64;
    let mut f = std::io::BufWriter::new(std::fs::File::create(&dst).expect("create output"));

    const HEADER: u32 = 8;
    // Header, then pixels, then the IFD — so the strip offset is known before the
    // directory that names it, without seeking back.
    let ifd_off = HEADER as u64 + data_len;
    f.write_all(b"II\x2a\x00").unwrap();
    f.write_all(&(ifd_off as u32).to_le_bytes()).unwrap();

    for px in hdr.data.chunks(3) {
        for c in px {
            f.write_all(&c.to_le_bytes()).unwrap();
        }
    }

    // 3 SHORTs do not fit in a 4-byte value field, so BitsPerSample and
    // SampleFormat point at little arrays placed after the IFD.
    let entries: u16 = 9;
    let after_ifd = ifd_off + 2 + entries as u64 * 12 + 4;
    let bits_off = after_ifd as u32;
    let fmt_off = bits_off + 6;

    let mut ifd = Vec::new();
    ifd.extend_from_slice(&entries.to_le_bytes());
    let mut tag = |id: u16, ty: u16, count: u32, value: u32| {
        ifd.extend_from_slice(&id.to_le_bytes());
        ifd.extend_from_slice(&ty.to_le_bytes());
        ifd.extend_from_slice(&count.to_le_bytes());
        // A SHORT that fits inline lives in the low half of the value field.
        ifd.extend_from_slice(&value.to_le_bytes());
    };
    const SHORT: u16 = 3;
    const LONG: u16 = 4;
    tag(256, LONG, 1, w); // ImageWidth
    tag(257, LONG, 1, h); // ImageLength
    tag(258, SHORT, 3, bits_off); // BitsPerSample -> [32,32,32]
    tag(259, SHORT, 1, 1); // Compression: none
    tag(262, SHORT, 1, 2); // PhotometricInterpretation: RGB
    tag(273, LONG, 1, HEADER); // StripOffsets
    tag(277, SHORT, 1, 3); // SamplesPerPixel
    tag(279, LONG, 1, data_len as u32); // StripByteCounts, one strip
    tag(339, SHORT, 3, fmt_off); // SampleFormat -> [3,3,3] = IEEE float
    ifd.extend_from_slice(&0u32.to_le_bytes()); // no next IFD
    for _ in 0..3 {
        ifd.extend_from_slice(&32u16.to_le_bytes());
    }
    for _ in 0..3 {
        ifd.extend_from_slice(&3u16.to_le_bytes());
    }
    f.write_all(&ifd).unwrap();
    f.flush().unwrap();
    drop(f);

    // Not "it was written" but "it reads back as the same image", through the
    // decoder the fixture exists to feed. A fixture that only this program can
    // read would be worse than none.
    let back = tohdr_portable::load_hdr(&dst).expect("read the fixture back");
    assert_eq!((back.width, back.height), (w, h), "geometry changed");
    let diff = back
        .data
        .iter()
        .zip(&hdr.data)
        .filter(|(a, b)| a != b)
        .count();
    println!(
        "wrote {} ({} bytes); reads back {}",
        dst.display(),
        std::fs::metadata(&dst).map(|m| m.len()).unwrap_or(0),
        if diff == 0 {
            "bit-identical".to_string()
        } else {
            format!("with {diff} differing samples — NOT lossless")
        }
    );
}
