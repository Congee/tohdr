//! Adversarial check of the exact band-placement formula used in
//! `read.rs::draw_band` / `render_banded_into`:
//!
//!   origin.y = (rows + y0) as f64 - h as f64
//!
//! For every band index i, with y0 = i*band_rows and rows = this band's
//! actual row count (band_rows, or the remainder on the last band), does the
//! resulting buffer contain exactly image rows [y0, y0+rows), in order, at
//! buffer rows [0, rows)? Tested against odd heights and band_rows values
//! that do NOT divide h evenly, including band_rows=1, band_rows>h, and
//! non-power-of-two splits, comparing byte-for-byte against a full single
//! shot decode.

use std::ffi::c_void;

use objc2_core_foundation::{CFString, CFURL, CFURLPathStyle, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    kCGColorSpaceExtendedLinearSRGB, CGBitmapContextCreate, CGBitmapInfo, CGColorSpace, CGContext,
    CGImage, CGImageAlphaInfo, CGImageByteOrderInfo, CGImageComponentInfo,
};
use objc2_image_io::CGImageSource;

fn load(path: &str) -> objc2_core_foundation::CFRetained<CGImage> {
    let cfpath = CFString::from_str(path);
    let url =
        CFURL::with_file_system_path(None, Some(&cfpath), CFURLPathStyle::CFURLPOSIXPathStyle, false)
            .unwrap();
    let isrc = unsafe { CGImageSource::with_url(&url, None) }.unwrap();
    let idx = unsafe { isrc.primary_image_index() };
    unsafe { isrc.image_at_index(idx, None) }.unwrap()
}

fn draw_full(image: &CGImage, cs: &CGColorSpace, w: usize, h: usize, stride: usize) -> Vec<u8> {
    let bitmap = CGBitmapInfo(
        CGImageComponentInfo::Float.0
            | CGImageByteOrderInfo::Order32Little.0
            | CGImageAlphaInfo::PremultipliedLast.0,
    );
    let mut buf = vec![0u8; stride * h];
    let ctx = unsafe {
        CGBitmapContextCreate(buf.as_mut_ptr() as *mut c_void, w, h, 32, stride, Some(cs), bitmap.0)
    }
    .unwrap();
    CGContext::draw_image(
        Some(&ctx),
        CGRect { origin: CGPoint { x: 0.0, y: 0.0 }, size: CGSize { width: w as f64, height: h as f64 } },
        Some(image),
    );
    buf
}

/// Exact copy of `draw_band`'s geometry from read.rs.
fn draw_band(
    image: &CGImage,
    cs: &CGColorSpace,
    w: usize,
    h: usize,
    stride: usize,
    buf: &mut [u8],
    y0: usize,
    rows: usize,
) {
    buf[..stride * rows].fill(0);
    let bitmap = CGBitmapInfo(
        CGImageComponentInfo::Float.0
            | CGImageByteOrderInfo::Order32Little.0
            | CGImageAlphaInfo::PremultipliedLast.0,
    );
    let ctx = unsafe {
        CGBitmapContextCreate(
            buf.as_mut_ptr() as *mut c_void,
            w,
            rows,
            32,
            stride,
            Some(cs),
            bitmap.0,
        )
    }
    .unwrap();
    CGContext::draw_image(
        Some(&ctx),
        CGRect {
            origin: CGPoint { x: 0.0, y: (rows + y0) as f64 - h as f64 },
            size: CGSize { width: w as f64, height: h as f64 },
        },
        Some(image),
    );
}

fn check(path: &str, band_rows: usize) -> bool {
    let image = load(path);
    let w = CGImage::width(Some(&image));
    let h = CGImage::height(Some(&image));
    let stride = w * 16;
    let cs = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceExtendedLinearSRGB })).unwrap();

    let full = draw_full(&image, &cs, w, h, stride);

    let mut ok = true;
    let mut y0 = 0usize;
    let mut band_idx = 0;
    while y0 < h {
        let rows = band_rows.min(h - y0);
        let mut buf = vec![0u8; stride * rows];
        draw_band(&image, &cs, w, h, stride, &mut buf, y0, rows);
        let expect = &full[y0 * stride..(y0 + rows) * stride];
        if buf != expect {
            // Find first differing row for a useful message.
            for r in 0..rows {
                let a = &buf[r * stride..(r + 1) * stride];
                let b = &expect[r * stride..(r + 1) * stride];
                if a != b {
                    println!(
                        "  MISMATCH band#{band_idx} y0={y0} rows={rows} (h={h}, band_rows={band_rows}): buffer row {r} (image row {}) differs from full-frame reference",
                        y0 + r
                    );
                    break;
                }
            }
            ok = false;
        }
        y0 += rows;
        band_idx += 1;
    }
    ok
}

fn main() {
    let cases: &[(&str, &[usize])] = &[
        ("out/odd_97.tiff", &[1, 2, 3, 7, 11, 13, 40, 50, 96, 97, 98, 200]),
        ("out/odd_3.tiff", &[1, 2, 3, 4, 100]),
        ("out/odd_1.tiff", &[1, 2, 5]),
    ];
    let mut all_ok = true;
    for (path, band_rows_list) in cases {
        if !std::path::Path::new(path).exists() {
            println!("skip {path}: not found");
            continue;
        }
        for &br in *band_rows_list {
            let ok = check(path, br);
            println!("{path} band_rows={br:<4} {}", if ok { "OK" } else { "FAIL" });
            all_ok &= ok;
        }
    }
    std::process::exit(if all_ok { 0 } else { 1 });
}
