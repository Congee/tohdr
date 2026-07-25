//! Where does the 3 s decode actually happen?
//!
//! Banding is free in total, but that has two very different explanations:
//! CoreGraphics may decode the whole frame on the first draw and serve the
//! rest from cache, or it may decode only the region each draw touches. The
//! first is irreducibly serial. The second means the bands are independent
//! work and can run on all ten cores.
//!
//! Per-band timings tell them apart: one huge band followed by near-zero, or
//! an even spread.

use std::ffi::c_void;
use std::time::Instant;

use objc2_core_foundation::{CFString, CFURL, CFURLPathStyle, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    kCGColorSpaceExtendedLinearSRGB, CGBitmapContextCreate, CGBitmapInfo, CGColorSpace, CGContext,
    CGImage, CGImageAlphaInfo, CGImageByteOrderInfo, CGImageComponentInfo,
};
use objc2_image_io::{kCGImageSourceDecodeRequest, kCGImageSourceDecodeToHDR, CGImageSource};

fn main() {
    let path = std::env::args().nth(1).expect("usage: probe_band_timing <file>");
    let bands: usize = std::env::args().nth(2).map_or(8, |s| s.parse().unwrap());
    let cfpath = CFString::from_str(&path);
    let url =
        CFURL::with_file_system_path(None, Some(&cfpath), CFURLPathStyle::CFURLPOSIXPathStyle, false)
            .unwrap();
    let k: &CFString = unsafe { kCGImageSourceDecodeRequest };
    let v: &CFString = unsafe { kCGImageSourceDecodeToHDR };
    let d = objc2_core_foundation::CFDictionary::<CFString, CFString>::from_slices(&[k], &[v]);
    let o: &objc2_core_foundation::CFDictionary = d.as_opaque();

    let isrc = unsafe { CGImageSource::with_url(&url, Some(o)) }.unwrap();
    let idx = unsafe { isrc.primary_image_index() };
    let image = unsafe { isrc.image_at_index(idx, Some(o)) }.unwrap();
    let w = CGImage::width(Some(&image));
    let h = CGImage::height(Some(&image));
    println!("{w}x{h}, {bands} bands, one fresh CGImage");

    let cs = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceExtendedLinearSRGB })).unwrap();
    let bitmap = CGBitmapInfo(
        CGImageComponentInfo::Float.0
            | CGImageByteOrderInfo::Order32Little.0
            | CGImageAlphaInfo::PremultipliedLast.0,
    );
    let band_rows = h.div_ceil(bands);
    let stride = w * 16;
    let mut buf = vec![0u8; stride * band_rows];

    for b in 0..bands {
        let y0 = b * band_rows;
        if y0 >= h {
            break;
        }
        let rows = band_rows.min(h - y0);
        let t = Instant::now();
        let ctx = unsafe {
            CGBitmapContextCreate(
                buf.as_mut_ptr() as *mut c_void, w, rows, 32, stride, Some(&cs), bitmap.0,
            )
        }
        .unwrap();
        CGContext::draw_image(
            Some(&ctx),
            CGRect {
                origin: CGPoint { x: 0.0, y: (rows + y0) as f64 - h as f64 },
                size: CGSize { width: w as f64, height: h as f64 },
            },
            Some(&image),
        );
        drop(ctx);
        println!("  band {b:>2} rows {y0:>5}..{:<5} {:8.1} ms", y0 + rows, t.elapsed().as_secs_f64() * 1000.0);
    }
}
