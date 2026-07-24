//! Spike: exercise Apple ImageIO gain-map READ and WRITE through Rust FFI.
//!
//! Goal 1 (read): open IMG_4913.HEIC, dump the HDR/ISO gain-map auxiliary
//! data info dicts and a couple of Apple MakerNote tags.
//!
//! Goal 2 (write): decode the primary image, re-attach the ISO gain map we
//! read back, and write out a new HEIC with `kCGImageDestinationEncodeToISOGainmap`.

use std::ffi::c_void;
use std::path::Path;

use objc2_core_foundation::{
    CFDictionary, CFNumber, CFNumberType, CFRetained, CFString, CFType, CFURL, CFURLPathStyle,
};
use objc2_image_io::{
    kCGImageAuxiliaryDataInfoData, kCGImageAuxiliaryDataInfoDataDescription,
    kCGImageAuxiliaryDataTypeHDRGainMap, kCGImageAuxiliaryDataTypeISOGainMap,
    kCGImageDestinationEncodeToISOGainmap, kCGImagePropertyMakerAppleDictionary, CGImageDestination,
    CGImageSource,
};

const INPUT: &str = "~/Downloads/IMG_4913.HEIC";
const OUTPUT: &str = "~/dev/tohdr/out/engineA_iso.heic";

/// Cast a raw CF void pointer (as returned by `CFDictionary::keys_and_values`
/// / `CFDictionary::value`) to a `&CFType`. Every CF object is a valid
/// `CFType` reference; `CFType::downcast_ref` narrows it further.
///
/// # Safety
/// `ptr` must be a non-null, live CoreFoundation object pointer.
unsafe fn cf_ref<'a>(ptr: *const c_void) -> &'a CFType {
    unsafe { &*(ptr as *const CFType) }
}

/// Best-effort "print this CF value" for the DataDescription dump: numbers as
/// i64/f64, strings as strings, booleans as bool, anything else falls back to
/// its native `CFCopyDescription` (via `{:?}`, wired up by objc2's `cf_type!`
/// macro on every CF type).
unsafe fn describe_value(ptr: *const c_void) -> String {
    let cf = unsafe { cf_ref(ptr) };
    if let Some(s) = cf.downcast_ref::<CFString>() {
        return format!("{s:?} (CFString)");
    }
    if let Some(n) = cf.downcast_ref::<CFNumber>() {
        let mut v: f64 = 0.0;
        unsafe { n.value(CFNumberType::Float64Type, &mut v as *mut f64 as *mut c_void) };
        if n.is_float_type() {
            return format!("{v} (CFNumber f64)");
        }
        return format!("{} (CFNumber i64, type={:?})", v as i64, n.r#type());
    }
    // Fallback: whatever it is, print its native description.
    format!("{cf:?}")
}

/// Dump every top-level key/value pair of a CFDictionary using our own
/// key/value walk (rather than relying solely on the native description),
/// so we can format numbers cleanly. We also print `{:?}` of the whole dict
/// beforehand so nothing the walk misses is lost.
fn dump_dict(label: &str, dict: &CFDictionary) {
    println!("  -- {label} (native description) --");
    println!("  {dict:?}");
    let count = dict.count() as usize;
    println!("  -- {label} ({count} keys, itemized) --");
    let mut keys: Vec<*const c_void> = vec![std::ptr::null(); count];
    let mut values: Vec<*const c_void> = vec![std::ptr::null(); count];
    unsafe { dict.keys_and_values(keys.as_mut_ptr(), values.as_mut_ptr()) };
    for (k, v) in keys.iter().zip(values.iter()) {
        let key_str = unsafe {
            cf_ref(*k)
                .downcast_ref::<CFString>()
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("{:?}", cf_ref(*k)))
        };
        let val_str = unsafe { describe_value(*v) };
        println!("    {key_str} = {val_str}");
    }
}

fn dump_aux_data_info(isrc: &CGImageSource, index: usize, aux_type_name: &str, aux_type: &CFString) {
    println!("=== auxiliary_data_info_at_index(0, {aux_type_name}) ===");
    let dict = unsafe { isrc.auxiliary_data_info_at_index(index, aux_type) };
    match dict {
        None => println!("  -> None (not present)"),
        Some(dict) => {
            println!("  -> Some, top-level keys:");
            let count = dict.count() as usize;
            let mut keys: Vec<*const c_void> = vec![std::ptr::null(); count];
            let mut values: Vec<*const c_void> = vec![std::ptr::null(); count];
            unsafe { dict.keys_and_values(keys.as_mut_ptr(), values.as_mut_ptr()) };
            for k in &keys {
                let key_str = unsafe {
                    cf_ref(*k)
                        .downcast_ref::<CFString>()
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| format!("{:?}", cf_ref(*k)))
                };
                println!("    - {key_str}");
            }

            // kCGImageAuxiliaryDataInfoData byte length.
            let data_key: &CFString = unsafe { kCGImageAuxiliaryDataInfoData };
            let data_ptr = unsafe { dict.value(data_key as *const CFString as *const c_void) };
            if data_ptr.is_null() {
                println!("  kCGImageAuxiliaryDataInfoData: <missing>");
            } else {
                let data = unsafe { cf_ref(data_ptr) }
                    .downcast_ref::<objc2_core_foundation::CFData>()
                    .expect("aux data value was not a CFData");
                println!("  kCGImageAuxiliaryDataInfoData: {} bytes", data.length());
            }

            // kCGImageAuxiliaryDataInfoDataDescription full dump.
            let desc_key: &CFString = unsafe { kCGImageAuxiliaryDataInfoDataDescription };
            let desc_ptr = unsafe { dict.value(desc_key as *const CFString as *const c_void) };
            if desc_ptr.is_null() {
                println!("  kCGImageAuxiliaryDataInfoDataDescription: <missing>");
            } else {
                let desc_dict = unsafe { cf_ref(desc_ptr) }
                    .downcast_ref::<CFDictionary>()
                    .expect("DataDescription value was not a CFDictionary");
                dump_dict("kCGImageAuxiliaryDataInfoDataDescription", desc_dict);
            }
        }
    }
    println!();
}

fn goal1_read(isrc: &CGImageSource) -> Option<CFRetained<CFDictionary>> {
    println!("################ GOAL 1: READ ################\n");

    let hdr_key: &'static CFString = unsafe { kCGImageAuxiliaryDataTypeHDRGainMap };
    let iso_key: &'static CFString = unsafe { kCGImageAuxiliaryDataTypeISOGainMap };

    dump_aux_data_info(isrc, 0, "kCGImageAuxiliaryDataTypeHDRGainMap", hdr_key);
    let iso_dict = unsafe { isrc.auxiliary_data_info_at_index(0, iso_key) };
    dump_aux_data_info(isrc, 0, "kCGImageAuxiliaryDataTypeISOGainMap", iso_key);

    // Overall image properties: dimensions, bit depth, MakerApple dict keys 33/48.
    println!("=== overall image properties (index 0) ===");
    let props = unsafe { isrc.properties_at_index(0, None) };
    if let Some(props) = props {
        let width_key = CFString::from_str("PixelWidth");
        let height_key = CFString::from_str("PixelHeight");
        let depth_key = CFString::from_str("Depth");
        for (label, key) in [
            ("PixelWidth", &width_key),
            ("PixelHeight", &height_key),
            ("Depth", &depth_key),
        ] {
            let key: &CFString = key;
            let ptr = unsafe { props.value(key as *const CFString as *const c_void) };
            if ptr.is_null() {
                println!("  {label}: <missing>");
            } else {
                println!("  {label}: {}", unsafe { describe_value(ptr) });
            }
        }

        let maker_key: &'static CFString = unsafe { kCGImagePropertyMakerAppleDictionary };
        let maker_ptr = unsafe { props.value(maker_key as *const CFString as *const c_void) };
        if maker_ptr.is_null() {
            println!("  kCGImagePropertyMakerAppleDictionary: <missing>");
        } else {
            let maker_dict = unsafe { cf_ref(maker_ptr) }
                .downcast_ref::<CFDictionary>()
                .expect("MakerApple value was not a CFDictionary");
            println!("  kCGImagePropertyMakerAppleDictionary present, {} keys", maker_dict.count());
            for tag in ["33", "48"] {
                let tag_key = CFString::from_str(tag);
                let tag_key_ref: &CFString = &tag_key;
                let tag_ptr = unsafe { maker_dict.value(tag_key_ref as *const CFString as *const c_void) };
                if tag_ptr.is_null() {
                    println!("    Apple tag \"{tag}\": <missing>");
                } else {
                    println!("    Apple tag \"{tag}\": {}", unsafe { describe_value(tag_ptr) });
                }
            }
        }
    } else {
        println!("  properties_at_index(0) returned None");
    }
    println!();

    iso_dict
}

fn goal2_write(isrc: &CGImageSource, iso_gain_map: Option<&CFDictionary>) {
    println!("################ GOAL 2: WRITE ################\n");

    let Some(iso_gain_map) = iso_gain_map else {
        println!("BLOCKED: no ISO gain map was read in Goal 1, nothing to re-attach.");
        return;
    };

    // Decode the primary image.
    let image = unsafe { isrc.image_at_index(0, None) };
    let Some(image) = image else {
        println!("BLOCKED: CGImageSourceCreateImageAtIndex returned NULL.");
        return;
    };
    println!(
        "decoded primary image: {}x{}, {} bpp",
        objc2_core_graphics::CGImage::width(Some(&image)),
        objc2_core_graphics::CGImage::height(Some(&image)),
        objc2_core_graphics::CGImage::bits_per_pixel(Some(&image))
    );

    // Destination: write straight to the output file via CFURL.
    let out_path = CFString::from_str(OUTPUT);
    let url = CFURL::with_file_system_path(None, Some(&out_path), CFURLPathStyle::CFURLPOSIXPathStyle, false)
        .expect("failed to build output CFURL");
    let heic_type = CFString::from_str("public.heic");

    let dest = unsafe { CGImageDestination::with_url(&url, &heic_type, 1, None) };
    let Some(dest) = dest else {
        println!("BLOCKED: CGImageDestinationCreateWithURL returned NULL (type not supported?).");
        return;
    };

    // Destination properties: request ISO gain-map encoding.
    let encode_key: &'static CFString = unsafe { kCGImageDestinationEncodeToISOGainmap };
    let true_num = unsafe {
        CFNumber::new(None, CFNumberType::SInt32Type, &1i32 as *const i32 as *const c_void)
    }
    .expect("failed to build CFNumber(1)");
    let props_dict = CFDictionary::<CFType, CFType>::from_slices(
        &[encode_key.as_ref()],
        &[true_num.as_ref()],
    );
    unsafe { dest.set_properties(Some(props_dict.as_opaque())) };

    unsafe { dest.add_image(&image, None) };

    let iso_key: &'static CFString = unsafe { kCGImageAuxiliaryDataTypeISOGainMap };
    unsafe { dest.add_auxiliary_data_info(iso_key, iso_gain_map) };

    let ok = unsafe { dest.finalize() };
    println!("CGImageDestinationFinalize -> {ok}");
    if ok {
        let meta = std::fs::metadata(OUTPUT).expect("output file missing after finalize");
        println!("wrote {} bytes to {OUTPUT}", meta.len());
    } else {
        println!("BLOCKED: finalize() reported failure.");
    }
}

fn main() {
    assert!(Path::new(INPUT).is_file(), "input HEIC not found at {INPUT}");

    let input_path = CFString::from_str(INPUT);
    let url = CFURL::with_file_system_path(None, Some(&input_path), CFURLPathStyle::CFURLPOSIXPathStyle, false)
        .expect("failed to build input CFURL");
    let isrc = unsafe { CGImageSource::with_url(&url, None) }.expect("CGImageSourceCreateWithURL returned NULL");

    println!("opened {INPUT}, type = {:?}", unsafe { isrc.r#type() });
    println!("image count = {}\n", unsafe { isrc.count() });

    let iso_dict = goal1_read(&isrc);
    goal2_write(&isrc, iso_dict.as_deref());
}
