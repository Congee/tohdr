//! Scratch: report what `load_hdr` actually decodes from a source file, so a
//! colour-space misread shows up as numbers instead of a plausible-looking
//! image. Safe to delete.

use std::path::PathBuf;

fn main() {
    let path = PathBuf::from(std::env::args().nth(1).expect("usage: dump_hdr <file>"));
    let hdr = tohdr_portable::load_hdr(&path).expect("load_hdr");

    let mut min = f32::INFINITY;
    let mut max = f32::NEG_INFINITY;
    let mut above = 0usize;
    let n = (hdr.width * hdr.height) as usize;
    for i in 0..n {
        let l = hdr.luma(i as u32 % hdr.width, i as u32 / hdr.width);
        min = min.min(l);
        max = max.max(l);
        if l > 1.0 {
            above += 1;
        }
    }
    // How many distinct values live above white tells us whether the decoder
    // preserved the highlight gradient or crushed it all to one clamped code.
    let mut distinct_above: Vec<u32> = Vec::new();
    for i in 0..n {
        let l = hdr.luma(i as u32 % hdr.width, i as u32 / hdr.width);
        if l > 1.0 {
            distinct_above.push(l.to_bits());
        }
    }
    distinct_above.sort_unstable();
    distinct_above.dedup();

    println!("{}: {}x{}", path.display(), hdr.width, hdr.height);
    println!("  luma min {min:.6}  max {max:.6}  ({:.4} stops)", max.log2());
    println!("  pixels above 1.0: {above} of {n}");
    println!("  distinct luma values above 1.0: {}", distinct_above.len());
    println!("  peak_luma(0.001) = {:.6}", hdr.peak_luma(0.001));
}
