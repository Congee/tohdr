//! Encoding through ImageIO.
//!
//! Two routes, because they answer different questions:
//!
//! - [`encode_from_hdr`] hands ImageIO an extended-range HDR `CGImage` and asks
//!   it to author the whole file (`kCGImageDestinationEncodeToISOGainmap`).
//!   Apple derives the tone-mapped base and the gain plane itself. This is the
//!   *reference* path: whatever it emits is by definition what the Apple
//!   ecosystem expects, so it doubles as the oracle for Engine B's container.
//! - [`encode_parts`] attaches a base and a gain plane we already derived, so
//!   both engines can be measured on identical inputs. Without it an "engine
//!   comparison" would be comparing two different derivations as well as two
//!   different containers.

use std::ffi::c_void;
use std::ptr::NonNull;

use objc2_core_foundation::{
    CFBoolean, CFData, CFDictionary, CFMutableData, CFNumber, CFNumberType, CFRetained, CFString,
    CFType,
};
use objc2_core_graphics::{
    CGBitmapInfo, CGColorRenderingIntent, CGColorSpace, CGDataProvider, CGImage,
    CGImageAlphaInfo, CGImageByteOrderInfo, CGImageComponentInfo,
    kCGColorSpaceExtendedLinearSRGB, kCGColorSpaceSRGB,
};
use objc2_image_io::{
    kCGImageDestinationEncodeRequest, kCGImageDestinationEncodeToISOGainmap,
    kCGImageDestinationLossyCompressionQuality, CGImageDestination,
};
use tohdr_core::{HdrRgb, Rgb};

use crate::{Error, Result};

const HEIC_UTI: &str = "public.heic";

fn cf_num_f64(v: f64) -> CFRetained<CFNumber> {
    unsafe { CFNumber::new(None, CFNumberType::Float64Type, &v as *const f64 as *const c_void) }
        .expect("CFNumberCreate never returns NULL for a Float64")
}

/// Build an untyped `CFDictionary` from `CFString` keys and arbitrary CF values.
///
/// The ImageIO option dictionaries mix `CFNumber`, `CFBoolean` and `CFString`
/// values, which the typed `CFDictionary<K, V>` helpers cannot express.
fn cf_dict(pairs: &[(&CFString, &CFType)]) -> CFRetained<CFDictionary> {
    let keys: Vec<&CFString> = pairs.iter().map(|(k, _)| *k).collect();
    let vals: Vec<&CFType> = pairs.iter().map(|(_, v)| *v).collect();
    let d = CFDictionary::<CFString, CFType>::from_slices(&keys, &vals);
    // Erase the key/value types; ImageIO takes a plain CFDictionaryRef.
    unsafe { CFRetained::retain(NonNull::from(d.as_opaque())) }
}

/// Wrap linear extended-range float RGB in a `CGImage`.
///
/// `kCGColorSpaceExtendedLinearSRGB` is the space [`HdrRgb`] is defined in:
/// linear light, `1.0` at SDR diffuse white, values above it permitted. Using
/// a non-extended space here would clamp the headroom away before ImageIO ever
/// sees it.
fn cg_image_from_hdr(hdr: &HdrRgb) -> Result<CFRetained<CGImage>> {
    let bytes: Vec<u8> = hdr.data.iter().flat_map(|v| v.to_ne_bytes()).collect();
    let data = CFData::from_bytes(&bytes);
    let provider = CGDataProvider::with_cf_data(Some(&data))
        .ok_or(Error::NullFromFramework("CGDataProviderCreateWithCFData"))?;
    let cs = unsafe { CGColorSpace::with_name(Some(kCGColorSpaceExtendedLinearSRGB)) }
        .ok_or(Error::NullFromFramework("CGColorSpaceCreateWithName"))?;

    // Float samples, no alpha, little-endian — spelled through the component
    // and byte-order enums because the flat `CGBitmapInfo` aliases for these
    // two bits are deprecated.
    let bitmap = CGBitmapInfo(
        CGImageComponentInfo::Float.0 | CGImageByteOrderInfo::Order32Little.0
            | CGImageAlphaInfo::None.0,
    );

    unsafe {
        CGImage::new(
            hdr.width as usize,
            hdr.height as usize,
            32,
            96,
            hdr.width as usize * 12,
            Some(&cs),
            bitmap,
            Some(&provider),
            std::ptr::null(),
            false,
            CGColorRenderingIntent::RenderingIntentDefault,
        )
    }
    .ok_or(Error::NullFromFramework("CGImageCreate (hdr)"))
}

/// Wrap 8-bit sRGB RGB in a `CGImage`.
fn cg_image_from_sdr(rgb: &Rgb) -> Result<CFRetained<CGImage>> {
    if rgb.bits != 8 {
        return Err(Error::Unreadable(format!(
            "Engine A's SDR path takes 8-bit input, got {}-bit",
            rgb.bits
        )));
    }
    let bytes: Vec<u8> = rgb.data.iter().map(|&v| v as u8).collect();
    let data = CFData::from_bytes(&bytes);
    let provider = CGDataProvider::with_cf_data(Some(&data))
        .ok_or(Error::NullFromFramework("CGDataProviderCreateWithCFData"))?;
    let cs = unsafe { CGColorSpace::with_name(Some(kCGColorSpaceSRGB)) }
        .ok_or(Error::NullFromFramework("CGColorSpaceCreateWithName"))?;
    unsafe {
        CGImage::new(
            rgb.width as usize,
            rgb.height as usize,
            8,
            24,
            rgb.width as usize * 3,
            Some(&cs),
            CGBitmapInfo(CGImageAlphaInfo::None.0),
            Some(&provider),
            std::ptr::null(),
            false,
            CGColorRenderingIntent::RenderingIntentDefault,
        )
    }
    .ok_or(Error::NullFromFramework("CGImageCreate (sdr)"))
}

/// Let ImageIO author a complete ISO gain-map HEIC from an HDR image.
///
/// Apple picks the tone curve, the gain plane and every container detail, so
/// the result is the ground truth for "what a gain-map HEIC should look like on
/// this OS". `quality` is `1..=100`, mapped onto ImageIO's `0.0..=1.0`.
pub fn encode_from_hdr(hdr: &HdrRgb, quality: u8) -> Result<Vec<u8>> {
    let image = cg_image_from_hdr(hdr)?;

    let out = CFMutableData::new(None, 0).ok_or(Error::NullFromFramework("CFDataCreateMutable"))?;
    let uti = CFString::from_str(HEIC_UTI);
    let dest = unsafe { CGImageDestination::with_data(&out, &uti, 1, None) }
        .ok_or(Error::NullFromFramework("CGImageDestinationCreateWithData"))?;

    let q = cf_num_f64((quality.clamp(1, 100) as f64) / 100.0);
    let request: &CFString = unsafe { kCGImageDestinationEncodeToISOGainmap };
    let opts = cf_dict(&[
        (
            unsafe { kCGImageDestinationEncodeRequest },
            request.as_ref(),
        ),
        (
            unsafe { kCGImageDestinationLossyCompressionQuality },
            q.as_ref(),
        ),
    ]);

    unsafe { dest.add_image(&image, Some(&opts)) };
    if !unsafe { dest.finalize() } {
        return Err(Error::FinalizeFailed);
    }
    Ok(out.to_vec())
}

/// Attach a gain plane we derived ourselves to an SDR base, through ImageIO.
///
/// Used by the engine comparison so Engine A and Engine B encode the *same*
/// derived inputs. `_meta` is currently unused: ImageIO recomputes the ISO
/// parameters from the plane it is handed rather than accepting ours, which is
/// itself a finding worth keeping visible rather than papering over.
pub fn encode_parts(base: &Rgb, quality: u8) -> Result<Vec<u8>> {
    let image = cg_image_from_sdr(base)?;
    let out = CFMutableData::new(None, 0).ok_or(Error::NullFromFramework("CFDataCreateMutable"))?;
    let uti = CFString::from_str(HEIC_UTI);
    let dest = unsafe { CGImageDestination::with_data(&out, &uti, 1, None) }
        .ok_or(Error::NullFromFramework("CGImageDestinationCreateWithData"))?;

    let q = cf_num_f64((quality.clamp(1, 100) as f64) / 100.0);
    let yes = CFBoolean::new(true);
    let opts = cf_dict(&[
        (
            unsafe { kCGImageDestinationLossyCompressionQuality },
            q.as_ref(),
        ),
        (
            unsafe { objc2_image_io::kCGImageDestinationPreserveGainMap },
            yes.as_ref(),
        ),
    ]);
    unsafe { dest.add_image(&image, Some(&opts)) };
    if !unsafe { dest.finalize() } {
        return Err(Error::FinalizeFailed);
    }
    Ok(out.to_vec())
}
