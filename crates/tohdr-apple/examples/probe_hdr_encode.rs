//! Scratch: why does `encode_from_hdr` declare zero headroom?
//!
//! It hands ImageIO an extended-linear-sRGB float `CGImage` and asks for
//! `kCGImageDestinationEncodeToISOGainmap`, and gets back a well-formed file
//! whose gain map carries nothing. Candidate causes, each tried below:
//!
//!   A. 96-bit RGB float with `AlphaInfo::None` is not a layout CoreGraphics
//!      actually honours, so the pixels ImageIO sees are not the ones we wrote.
//!   B. It is honoured, but ImageIO needs an explicit content headroom hint
//!      before it will derive a non-trivial gain map.
//!   C. The colour space is wrong for the request.
//!
//! Prints the declared headroom for each variant; whichever is non-zero is the
//! answer. Safe to delete once resolved.

use std::ffi::c_void;
use std::ptr::NonNull;

use objc2_core_foundation::{
    CFData, CFDictionary, CFMutableData, CFNumber, CFNumberType, CFRetained, CFString, CFType,
};
use objc2_core_graphics::{
    kCGColorSpaceExtendedLinearDisplayP3, kCGColorSpaceExtendedLinearSRGB, CGBitmapInfo,
    CGColorRenderingIntent, CGColorSpace, CGDataProvider, CGImage, CGImageAlphaInfo,
    CGImageByteOrderInfo, CGImageComponentInfo,
};
use objc2_image_io::{
    kCGImageDestinationEncodeRequest, kCGImageDestinationEncodeToISOGainmap,
    kCGImageDestinationLossyCompressionQuality, CGImageDestination,
};
use tohdr_core::HdrRgb;

fn cf_num(v: f64) -> CFRetained<CFNumber> {
    unsafe { CFNumber::new(None, CFNumberType::Float64Type, &v as *const f64 as *const c_void) }
        .unwrap()
}

fn cf_dict(pairs: &[(&CFString, &CFType)]) -> CFRetained<CFDictionary> {
    let keys: Vec<&CFString> = pairs.iter().map(|(k, _)| *k).collect();
    let vals: Vec<&CFType> = pairs.iter().map(|(_, v)| *v).collect();
    let d = CFDictionary::<CFString, CFType>::from_slices(&keys, &vals);
    unsafe { CFRetained::retain(NonNull::from(d.as_opaque())) }
}

/// Build a float CGImage. `alpha` picks the layout: `None` gives 3 components
/// at 96 bpp, anything else gives 4 at 128 bpp with the extra channel set to
/// 1.0.
fn make_image(
    hdr: &HdrRgb,
    alpha: CGImageAlphaInfo,
    p3: bool,
) -> Option<CFRetained<CGImage>> {
    let with_alpha = !matches!(alpha, CGImageAlphaInfo::None);
    let comps = if with_alpha { 4 } else { 3 };
    let mut samples: Vec<f32> = Vec::with_capacity((hdr.width * hdr.height) as usize * comps);
    for i in 0..(hdr.width * hdr.height) as usize {
        samples.extend_from_slice(&hdr.data[i * 3..i * 3 + 3]);
        if with_alpha {
            samples.push(1.0);
        }
    }
    let bytes: Vec<u8> = samples.iter().flat_map(|v| v.to_ne_bytes()).collect();
    let data = CFData::from_bytes(&bytes);
    let provider = CGDataProvider::with_cf_data(Some(&data))?;
    let name = if p3 {
        unsafe { kCGColorSpaceExtendedLinearDisplayP3 }
    } else {
        unsafe { kCGColorSpaceExtendedLinearSRGB }
    };
    let cs = unsafe { CGColorSpace::with_name(Some(name)) }?;
    let bitmap = CGBitmapInfo(
        CGImageComponentInfo::Float.0 | CGImageByteOrderInfo::Order32Little.0 | alpha.0,
    );
    unsafe {
        CGImage::new(
            hdr.width as usize,
            hdr.height as usize,
            32,
            32 * comps,
            hdr.width as usize * 4 * comps,
            Some(&cs),
            bitmap,
            Some(&provider),
            std::ptr::null(),
            false,
            CGColorRenderingIntent::RenderingIntentDefault,
        )
    }
}

fn encode(image: &CGImage) -> Option<Vec<u8>> {
    let out = CFMutableData::new(None, 0)?;
    let uti = CFString::from_str("public.heic");
    let dest = unsafe { CGImageDestination::with_data(&out, &uti, 1, None) }?;
    let q = cf_num(0.85);
    let req: &CFString = unsafe { kCGImageDestinationEncodeToISOGainmap };
    let opts = cf_dict(&[
        (unsafe { kCGImageDestinationEncodeRequest }, req.as_ref()),
        (unsafe { kCGImageDestinationLossyCompressionQuality }, q.as_ref()),
    ]);
    unsafe { dest.add_image(image, Some(&opts)) };
    if !unsafe { dest.finalize() } {
        return None;
    }
    Some(out.to_vec())
}

fn report(label: &str, image: Option<CFRetained<CGImage>>) {
    let Some(image) = image else {
        println!("{label:<44} CGImageCreate returned NULL");
        return;
    };
    let Some(bytes) = encode(&image) else {
        println!("{label:<44} finalize failed");
        return;
    };
    match tohdr_apple::inspect_bytes(&bytes) {
        Err(e) => println!("{label:<44} read-back error: {e}"),
        Ok(rb) => {
            let hr = rb.iso_meta.map(|m| m.alt_headroom).unwrap_or(f32::NAN);
            // Decode the base back and report its mean luma. If a layout is
            // being misread, the pixels ImageIO stored are not ours and this
            // number will not match the source's tone-mapped mean.
            let path = std::path::Path::new("out").join(format!(
                "probe_{}.heic",
                label.chars().next().unwrap_or('x')
            ));
            std::fs::write(&path, &bytes).ok();
            let mean = tohdr_apple::load_sdr(&path)
                .map(|b| {
                    let s: f64 = b.data.iter().map(|&v| v as f64).sum();
                    s / b.data.len() as f64
                })
                .unwrap_or(f64::NAN);
            println!(
                "{label:<44} {:>8} B  iso={}  headroom={hr:.4}  base mean={mean:.1}/255",
                bytes.len(),
                rb.iso_aux
            );
        }
    }
}

fn main() {
    let src = std::env::args().nth(1).unwrap_or_else(|| "out/scene.tiff".into());
    let hdr = tohdr_portable::load_hdr(std::path::Path::new(&src)).expect("load");
    println!(
        "source {}x{}, peak {:.3}x ({:.3} stops)\n",
        hdr.width,
        hdr.height,
        hdr.peak_luma(0.001),
        hdr.peak_luma(0.001).log2()
    );

    report("A: 96bpp RGB, AlphaNone, linear sRGB", make_image(&hdr, CGImageAlphaInfo::None, false));
    report(
        "B: 128bpp RGBA, PremultipliedLast, lin sRGB",
        make_image(&hdr, CGImageAlphaInfo::PremultipliedLast, false),
    );
    report(
        "C: 128bpp RGBX, NoneSkipLast, linear sRGB",
        make_image(&hdr, CGImageAlphaInfo::NoneSkipLast, false),
    );
    report(
        "D: 128bpp RGBX, NoneSkipLast, linear P3",
        make_image(&hdr, CGImageAlphaInfo::NoneSkipLast, true),
    );
}
