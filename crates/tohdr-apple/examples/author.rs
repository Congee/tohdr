//! `cargo run --example author -p tohdr-apple -- <hdr.tiff> <out.heic> [quality]`
//!
//! Have ImageIO author a gain-map HEIC from an HDR source, so its container can
//! be diffed against Engine B's.

fn main() {
    let mut a = std::env::args().skip(1);
    let src = a.next().expect("usage: author <hdr.tiff> <out.heic> [quality]");
    let dst = a.next().expect("usage: author <hdr.tiff> <out.heic> [quality]");
    let q: u8 = a.next().and_then(|s| s.parse().ok()).unwrap_or(85);

    let hdr = tohdr_portable::load_hdr(std::path::Path::new(&src)).expect("load hdr");
    let peak = hdr.peak_luma(0.001);
    println!("source {}x{}, peak {peak:.3}x SDR white", hdr.width, hdr.height);

    let bytes = tohdr_apple::encode_from_hdr(&hdr, q, tohdr_core::Primaries::DisplayP3).expect("encode_from_hdr");
    std::fs::write(&dst, &bytes).expect("write");
    println!("wrote {dst} ({} bytes) at quality {q}", bytes.len());

    match tohdr_apple::inspect_bytes(&bytes) {
        Err(e) => println!("read-back ERROR: {e}"),
        Ok(rb) => {
            println!(
                "read-back: {}x{} depth {} apple_aux={} iso_aux={} gain={:?} fmt={:?}",
                rb.width, rb.height, rb.depth, rb.apple_aux, rb.iso_aux, rb.gain_size,
                rb.gain_pixel_format
            );
            if let Some(m) = &rb.iso_meta {
                println!(
                    "  iso: base_hr={:.6} alt_hr={:.6} max_log2={:.6} gamma={:.6} min={:.6}",
                    m.base_headroom, m.alt_headroom, m.max_log2[0], m.gamma[0], m.min_log2[0]
                );
                println!("  headroom_consistent = {:?}", rb.headroom_consistent());
            }
        }
    }
}
