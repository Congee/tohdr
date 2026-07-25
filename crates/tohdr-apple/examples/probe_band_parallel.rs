//! Can bands be drawn concurrently?
//!
//! `probe_band_timing` split the 3.2 s render into a ~1180 ms one-off paid by
//! the first draw and ~250 ms per band of area-proportional work. Only the
//! second part can be spread over cores, and only if CoreGraphics lets several
//! threads draw one shared `CGImage` into their own contexts at once. `CGImage`
//! is documented immutable, but an internal lock around the decoded-bitmap
//! cache would serialise them anyway, so this measures rather than assumes.

use std::ffi::c_void;
use std::time::Instant;

use objc2_core_foundation::{
    CFDictionary, CFRetained, CFString, CFURL, CFURLPathStyle, CGPoint, CGRect, CGSize,
};
use objc2_core_graphics::{
    kCGColorSpaceExtendedLinearSRGB, CGBitmapContextCreate, CGBitmapInfo, CGColorSpace, CGContext,
    CGImage, CGImageAlphaInfo, CGImageByteOrderInfo, CGImageComponentInfo,
};
use objc2_image_io::{kCGImageSourceDecodeRequest, kCGImageSourceDecodeToHDR, CGImageSource};

/// `CGImage` and `CGColorSpace` are immutable Core Foundation objects; the
/// bindings simply do not assert it. Sharing them read-only across threads is
/// exactly what this probe is here to test.
struct Shared {
    image: CFRetained<CGImage>,
    cs: CFRetained<CGColorSpace>,
}
unsafe impl Send for Shared {}
unsafe impl Sync for Shared {}

fn main() {
    let path = std::env::args().nth(1).expect("usage: probe_band_parallel <file>");
    let cfpath = CFString::from_str(&path);
    let url =
        CFURL::with_file_system_path(None, Some(&cfpath), CFURLPathStyle::CFURLPOSIXPathStyle, false)
            .unwrap();
    let k: &CFString = unsafe { kCGImageSourceDecodeRequest };
    let v: &CFString = unsafe { kCGImageSourceDecodeToHDR };
    let d = CFDictionary::<CFString, CFString>::from_slices(&[k], &[v]);
    let o: &CFDictionary = d.as_opaque();

    let fresh = || {
        let isrc = unsafe { CGImageSource::with_url(&url, Some(o)) }.unwrap();
        let idx = unsafe { isrc.primary_image_index() };
        Shared {
            image: unsafe { isrc.image_at_index(idx, Some(o)) }.unwrap(),
            cs: CGColorSpace::with_name(Some(unsafe { kCGColorSpaceExtendedLinearSRGB })).unwrap(),
        }
    };
    let sh = fresh();
    let w = CGImage::width(Some(&sh.image));
    let h = CGImage::height(Some(&sh.image));
    let stride = w * 16;
    let bitmap = CGBitmapInfo(
        CGImageComponentInfo::Float.0
            | CGImageByteOrderInfo::Order32Little.0
            | CGImageAlphaInfo::PremultipliedLast.0,
    );
    println!("{w}x{h}");

    let draw = |sh: &Shared, buf: &mut [u8], y0: usize, rows: usize| {
        let ctx = unsafe {
            CGBitmapContextCreate(
                buf.as_mut_ptr() as *mut c_void, w, rows, 32, stride, Some(&sh.cs), bitmap.0,
            )
        }
        .unwrap();
        CGContext::draw_image(
            Some(&ctx),
            CGRect {
                origin: CGPoint { x: 0.0, y: (rows + y0) as f64 - h as f64 },
                size: CGSize { width: w as f64, height: h as f64 },
            },
            Some(&sh.image),
        );
    };

    drop(sh);

    // Every configuration gets its own `CGImage`, so none of them inherits a
    // warmed cache from the one before. The one-off decode is forced with a
    // one-row draw and reported separately; only the area-proportional work
    // that follows it is the thing threads can divide.
    for threads in [1usize, 2, 4, 8, 10] {
        let sh = fresh();
        let mut warm = vec![0u8; stride];
        let tw = Instant::now();
        draw(&sh, &mut warm, 0, 1);
        let warm_ms = tw.elapsed().as_secs_f64() * 1000.0;
        drop(warm);

        let band_rows = h.div_ceil(threads);
        let mut bufs: Vec<Vec<u8>> = (0..threads).map(|_| vec![0u8; stride * band_rows]).collect();
        let shr = &sh;
        let t = Instant::now();
        std::thread::scope(|s| {
            for (i, buf) in bufs.iter_mut().enumerate() {
                s.spawn(move || {
                    let y0 = i * band_rows;
                    if y0 < h {
                        draw(shr, buf, y0, band_rows.min(h - y0));
                    }
                });
            }
        });
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        println!("  {threads:>2} thread(s): decode {warm_ms:7.1} ms + draw {ms:7.1} ms = {:7.1} ms", warm_ms + ms);
    }
}
