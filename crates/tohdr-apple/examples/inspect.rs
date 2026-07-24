//! `cargo run --example inspect -p tohdr-apple -- <file>...`
//!
//! Prints what macOS ImageIO reports for each file, in the same terms the
//! acceptance criteria are written in.

fn fourcc(v: u32) -> String {
    let b = v.to_be_bytes();
    if b.iter().all(|c| c.is_ascii_graphic()) {
        format!("{} ({v})", String::from_utf8_lossy(&b))
    } else {
        format!("{v}")
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("usage: inspect <file>...");
        std::process::exit(2);
    }
    for path in &args {
        println!("=== {path} ===");
        match tohdr_apple::inspect(std::path::Path::new(path)) {
            Err(e) => println!("  ERROR: {e}"),
            Ok(rb) => {
                println!("  {}x{} depth {}", rb.width, rb.height, rb.depth);
                println!("  apple_aux={} iso_aux={}", rb.apple_aux, rb.iso_aux);
                match rb.gain_size {
                    Some((w, h)) => println!("  gain plane {w}x{h}"),
                    None => println!("  gain plane: none reported"),
                }
                match rb.gain_pixel_format {
                    Some(p) => println!("  gain pixel format {}", fourcc(p)),
                    None => println!("  gain pixel format: none"),
                }
                println!("  tag33={:?} tag48={:?}", rb.tag33, rb.tag48);
                println!("  apple_headroom={:?}", rb.apple_headroom);
                match &rb.iso_meta {
                    None => println!("  iso_meta: none"),
                    Some(m) => {
                        println!("  iso_meta:");
                        println!("    base_headroom = {:.6}", m.base_headroom);
                        println!("    alt_headroom  = {:.6}", m.alt_headroom);
                        println!("    min_log2[0]   = {:.6}", m.min_log2[0]);
                        println!("    max_log2[0]   = {:.6}", m.max_log2[0]);
                        println!("    gamma[0]      = {:.6}", m.gamma[0]);
                        println!("    base_offset[0]= {:.6}", m.base_offset[0]);
                        println!("    alt_offset[0] = {:.6}", m.alt_offset[0]);
                        println!("    use_base_cs   = {}", m.use_base_color_space);
                    }
                }
                println!("  headroom_consistent = {:?}", rb.headroom_consistent());
            }
        }
        println!();
    }
}
