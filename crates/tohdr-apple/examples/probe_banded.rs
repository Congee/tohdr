//! Is a banded `draw_image` viable?
//!
//! The full-frame render target is the single largest allocation in the
//! program (918 MiB at 60 Mpx) and it exists only to be de-interleaved into
//! `HdrRgb` and thrown away. Drawing in horizontal bands into a small reusable
//! buffer would delete it outright — *if* CoreGraphics decodes the source once
//! rather than once per band.
//!
//! That is the whole question, and it is cheap to answer: draw the same image
//! full-frame, then in N bands, and compare total time. Linear growth in N
//! means every band re-decodes and the idea is dead.

use std::ffi::c_void;
use std::time::Instant;

use objc2_core_foundation::{CFString, CFURL, CFURLPathStyle, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    kCGColorSpaceExtendedLinearSRGB, CGBitmapContextCreate, CGBitmapInfo, CGColorSpace, CGContext,
    CGImage, CGImageAlphaInfo, CGImageByteOrderInfo, CGImageComponentInfo,
};
use objc2_image_io::{kCGImageSourceDecodeRequest, kCGImageSourceDecodeToHDR, CGImageSource};

fn main() {
    let path = std::env::args().nth(1).expect("usage: probe_banded <file>");
    let cfpath = CFString::from_str(&path);
    let url = CFURL::with_file_system_path(
        None,
        Some(&cfpath),
        CFURLPathStyle::CFURLPOSIXPathStyle,
        false,
    )
    .unwrap();
    let k: &CFString = unsafe { kCGImageSourceDecodeRequest };
    let v: &CFString = unsafe { kCGImageSourceDecodeToHDR };
    let d = objc2_core_foundation::CFDictionary::<CFString, CFString>::from_slices(&[k], &[v]);
    let o: &objc2_core_foundation::CFDictionary = d.as_opaque();

    let fresh_image = || {
        let isrc = unsafe { CGImageSource::with_url(&url, Some(o)) }.unwrap();
        let idx = unsafe { isrc.primary_image_index() };
        unsafe { isrc.image_at_index(idx, Some(o)) }.unwrap()
    };
    let probe = fresh_image();
    let w = CGImage::width(Some(&probe));
    let h = CGImage::height(Some(&probe));
    drop(probe);
    println!("{w}x{h}  (each run below decodes from scratch)");

    let cs = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceExtendedLinearSRGB })).unwrap();
    let bitmap = CGBitmapInfo(
        CGImageComponentInfo::Float.0
            | CGImageByteOrderInfo::Order32Little.0
            | CGImageAlphaInfo::PremultipliedLast.0,
    );

    // Draw `bands` horizontal strips, translating the CTM so each strip lands
    // at the top of a band-sized context.
    let draw_banded = |bands: usize| -> (f64, usize) {
        let image = fresh_image();
        let band_rows = h.div_ceil(bands);
        let stride = w * 16;
        let mut buf = vec![0u8; stride * band_rows];
        let t = Instant::now();
        let mut done = 0usize;
        for b in 0..bands {
            let y0 = b * band_rows;
            if y0 >= h {
                break;
            }
            let rows = band_rows.min(h - y0);
            let ctx = unsafe {
                CGBitmapContextCreate(
                    buf.as_mut_ptr() as *mut c_void,
                    w,
                    rows,
                    32,
                    stride,
                    Some(&cs),
                    bitmap.0,
                )
            }
            .expect("band context");
            // CoreGraphics origin is bottom-left. Placing the full image at
            // this offset leaves exactly the wanted strip inside the context.
            let y_off = -((h - y0 - rows) as f64);
            CGContext::draw_image(
                Some(&ctx),
                CGRect {
                    origin: CGPoint { x: 0.0, y: y_off },
                    size: CGSize { width: w as f64, height: h as f64 },
                },
                Some(&image),
            );
            drop(ctx);
            done += rows;
        }
        (t.elapsed().as_secs_f64() * 1000.0, done)
    };

    for bands in [1usize, 2, 4, 16] {
        let (ms, rows) = draw_banded(bands);
        let mib = (w * 16 * h.div_ceil(bands)) / (1024 * 1024);
        println!("  {bands:>3} band(s): {ms:8.1} ms   buffer {mib:>4} MiB   rows drawn {rows}");
    }
}
