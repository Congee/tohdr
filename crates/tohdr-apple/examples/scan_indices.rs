//! Scratch: for every image index ImageIO reports, say which auxiliary data
//! types it carries. `inspect()` only looks at the primary index; if a gain map
//! is attached somewhere else we would otherwise never see it.

use std::ffi::c_void;

use objc2_core_foundation::{CFDictionary, CFString, CFType, CFURL, CFURLPathStyle};
use objc2_image_io::{
    kCGImageAuxiliaryDataInfoDataDescription, kCGImageAuxiliaryDataTypeHDRGainMap,
    kCGImageAuxiliaryDataTypeISOGainMap, CGImageSource,
};

fn get<'a>(dict: &'a CFDictionary, key: &CFString) -> Option<&'a CFType> {
    let ptr = unsafe { dict.value(key as *const CFString as *const c_void) };
    if ptr.is_null() { None } else { Some(unsafe { &*(ptr as *const CFType) }) }
}

fn main() {
    for path in std::env::args().skip(1) {
        println!("=== {path} ===");
        let cfpath = CFString::from_str(&path);
        let Some(url) = CFURL::with_file_system_path(
            None, Some(&cfpath), CFURLPathStyle::CFURLPOSIXPathStyle, false) else {
            println!("  bad url"); continue;
        };
        let Some(isrc) = (unsafe { CGImageSource::with_url(&url, None) }) else {
            println!("  ImageIO could not open it"); continue;
        };
        let n = unsafe { isrc.count() };
        let primary = unsafe { isrc.primary_image_index() };
        let ty = unsafe { isrc.r#type() }.map(|s| s.to_string());
        println!("  type={ty:?} count={n} primary={primary}");
        for i in 0..n {
            let apple = unsafe {
                isrc.auxiliary_data_info_at_index(i, kCGImageAuxiliaryDataTypeHDRGainMap)
            };
            let iso = unsafe {
                isrc.auxiliary_data_info_at_index(i, kCGImageAuxiliaryDataTypeISOGainMap)
            };
            let desc = iso.as_ref().or(apple.as_ref()).and_then(|d| {
                let k: &CFString = unsafe { kCGImageAuxiliaryDataInfoDataDescription };
                get(d, k).map(|v| format!("{v:?}"))
            });
            let dims = unsafe { isrc.properties_at_index(i, None) }
                .map(|p| format!("{p:?}").replace('\n', " "))
                .unwrap_or_default();
            let short: String = dims
                .split(';')
                .filter(|s| {
                    s.contains("PixelWidth")
                        || s.contains("PixelHeight")
                        || s.contains("Depth")
                        || s.contains("ProfileName")
                })
                .map(|s| s.trim().to_string())
                .collect::<Vec<_>>()
                .join(", ");
            println!(
                "  [{i}] apple={} iso={} | {short}",
                apple.is_some(),
                iso.is_some()
            );
            if let Some(d) = desc {
                println!("        gain desc: {}", d.replace('\n', " "));
            }
        }
    }
}
