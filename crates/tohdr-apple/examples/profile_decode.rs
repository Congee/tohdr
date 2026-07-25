//! `cargo run --release --example profile_decode -p tohdr-apple -- <file>`
//!
//! Splits `read::render` into its three costs — ImageIO's own decode, the
//! CoreGraphics draw into our bitmap, and the Rust de-interleave — because
//! "decode is 68% of the pipeline" is not actionable until you know which of
//! those three it is.
//!
//! Also compares 32-bit float against 16-bit half float for the render target:
//! the buffer is the largest allocation in the program, and halving its width
//! halves the memory traffic of both the draw and the read-back.

use std::ffi::c_void;
use std::time::Instant;

use objc2_core_foundation::{CFString, CFURL, CFURLPathStyle, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    kCGColorSpaceExtendedLinearSRGB, CGBitmapContextCreate, CGBitmapInfo, CGColorSpace, CGContext,
    CGImage, CGImageAlphaInfo, CGImageByteOrderInfo, CGImageComponentInfo,
};
use objc2_image_io::{
    kCGImageSourceDecodeRequest, kCGImageSourceDecodeToHDR, CGImageSource,
};

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

fn run(path: &str, bits: usize) {
    let cfpath = CFString::from_str(path);
    let url = CFURL::with_file_system_path(
        None,
        Some(&cfpath),
        CFURLPathStyle::CFURLPOSIXPathStyle,
        false,
    )
    .unwrap();

    let key: &CFString = unsafe { kCGImageSourceDecodeRequest };
    let val: &CFString = unsafe { kCGImageSourceDecodeToHDR };
    let opts = objc2_core_foundation::CFDictionary::<CFString, CFString>::from_slices(
        &[key],
        &[val],
    );
    let o: &objc2_core_foundation::CFDictionary = opts.as_opaque();

    let t0 = Instant::now();
    let isrc = unsafe { CGImageSource::with_url(&url, Some(o)) }.unwrap();
    let idx = unsafe { isrc.primary_image_index() };
    let open_ms = ms(t0);

    let t1 = Instant::now();
    let image = unsafe { isrc.image_at_index(idx, Some(o)) }.unwrap();
    let decode_ms = ms(t1);

    let w = CGImage::width(Some(&image));
    let h = CGImage::height(Some(&image));

    let bytes_per_px = (bits / 8) * 4;
    let stride = w * bytes_per_px;
    let t2 = Instant::now();
    let mut buf = vec![0u8; stride * h];
    let alloc_ms = ms(t2);

    let cs = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceExtendedLinearSRGB })).unwrap();
    let bitmap = CGBitmapInfo(
        CGImageComponentInfo::Float.0
            | CGImageByteOrderInfo::Order32Little.0
            | CGImageAlphaInfo::PremultipliedLast.0,
    );

    let t3 = Instant::now();
    let ctx = unsafe {
        CGBitmapContextCreate(
            buf.as_mut_ptr() as *mut c_void,
            w,
            h,
            bits,
            stride,
            Some(&cs),
            bitmap.0,
        )
    };
    let Some(ctx) = ctx else {
        println!("  {bits}-bit float: CGBitmapContextCreate returned NULL (unsupported)");
        return;
    };
    CGContext::draw_image(
        Some(&ctx),
        CGRect {
            origin: CGPoint { x: 0.0, y: 0.0 },
            size: CGSize { width: w as f64, height: h as f64 },
        },
        Some(&image),
    );
    drop(ctx);
    let draw_ms = ms(t3);

    // The de-interleave the library currently does: RGBA -> packed RGB f32.
    let t4 = Instant::now();
    let n = w * h;
    let mut out: Vec<f32> = Vec::with_capacity(n * 3);
    if bits == 32 {
        for i in 0..n {
            let o = i * 16;
            for c in 0..3 {
                let b = &buf[o + c * 4..o + c * 4 + 4];
                out.push(f32::from_ne_bytes([b[0], b[1], b[2], b[3]]).max(0.0));
            }
        }
    } else {
        for i in 0..n {
            let o = i * 8;
            for c in 0..3 {
                let b = u16::from_ne_bytes([buf[o + c * 2], buf[o + c * 2 + 1]]);
                out.push(half_to_f32(b).max(0.0));
            }
        }
    }
    let deinterleave_ms = ms(t4);

    let total = open_ms + decode_ms + alloc_ms + draw_ms + deinterleave_ms;
    println!(
        "  {bits}-bit float target ({} MiB buffer):",
        (stride * h) / (1024 * 1024)
    );
    println!("    open+index      {open_ms:8.1} ms");
    println!("    ImageIO decode  {decode_ms:8.1} ms");
    println!("    zeroed alloc    {alloc_ms:8.1} ms");
    println!("    CG draw_image   {draw_ms:8.1} ms");
    println!("    de-interleave   {deinterleave_ms:8.1} ms");
    println!("    ---- total      {total:8.1} ms");
    let peak = out.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
    println!("    peak sample     {peak:.4}");
}

/// IEEE 754 half -> f32, scalar. Only used to price the half-float path.
fn half_to_f32(h: u16) -> f32 {
    let sign = ((h >> 15) & 1) as u32;
    let exp = ((h >> 10) & 0x1F) as u32;
    let frac = (h & 0x3FF) as u32;
    let bits = if exp == 0 {
        if frac == 0 {
            sign << 31
        } else {
            // subnormal
            let mut e = -1i32;
            let mut f = frac;
            while f & 0x400 == 0 {
                f <<= 1;
                e -= 1;
            }
            let f = f & 0x3FF;
            ((sign << 31) | (((127 - 15 + e as i32) as u32) << 23) | (f << 13)) as u32
        }
    } else if exp == 0x1F {
        (sign << 31) | (0xFF << 23) | (frac << 13)
    } else {
        (sign << 31) | ((exp + 127 - 15) << 23) | (frac << 13)
    };
    f32::from_bits(bits)
}

fn main() {
    let path = std::env::args().nth(1).expect("usage: profile_decode <file>");
    println!("{path}");
    run(&path, 32);
    run(&path, 16);
}
