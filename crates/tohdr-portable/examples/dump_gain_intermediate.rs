//! Scratch: write hpvca's raw single-item gain-plane HEIC (before our muxer
//! touches it) so we can see what chroma/pixi the encoder itself produced.
//! Safe to delete.

use tohdr_core::GainPlane;

fn main() {
    let (w, h) = (512u32, 384u32);
    let data = (0..w * h).map(|i| (i % 256) as u8).collect();
    let gain = GainPlane { width: w, height: h, data };

    let bytes = tohdr_portable::debug_encode_gain_heic(&gain, 85).expect("encode");
    let path = std::env::args().nth(1).unwrap_or_else(|| "out/hpvca_gain_raw.heic".into());
    std::fs::write(&path, &bytes).expect("write");
    println!("wrote {path} ({} bytes)", bytes.len());

    let f = tohdr_heif::HeifFile::parse(&bytes).expect("parse");
    let id = f.primary_item().expect("pitm");
    let coded = f.coded_image(id).expect("coded");
    println!(
        "our reader sees: {}x{} depth {} chroma {:?}",
        coded.width, coded.height, coded.bit_depth, coded.chroma
    );
}
