//! Scratch probe (not part of the public API): can we *build* the
//! `HDRToneMap` metadata tree from scratch with `CGMutableImageMetadata` +
//! path-based setters, matching what `probe_meta` read back from
//! the reference capture? Informs `AppleEngine::encode`.
use std::ffi::c_void;

use objc2_core_foundation::{CFArray, CFDictionary, CFNumber, CFNumberType, CFRetained, CFString, CFType};
use objc2_image_io::{CGImageMetadataTag, CGImageMetadataType, CGMutableImageMetadata};

fn main() {
    let md: CFRetained<CGMutableImageMetadata> = unsafe { CGMutableImageMetadata::new() };

    let ns = CFString::from_str("http://ns.apple.com/HDRToneMap/1.0/");
    let prefix = CFString::from_str("HDRToneMap");
    let ok = unsafe { md.register_namespace_for_prefix(&ns, &prefix, std::ptr::null_mut()) };
    println!("register_namespace_for_prefix -> {ok}");

    let set_num = |path: &str, v: f64| {
        let p = CFString::from_str(path);
        let n = unsafe {
            CFNumber::new(None, CFNumberType::Float64Type, &v as *const f64 as *const c_void)
        }
        .unwrap();
        let ok = unsafe { md.set_value_with_path(None, &p, n.as_ref()) };
        println!("set {path} = {v} -> {ok}");
    };

    set_num("HDRToneMap:Version", 1.0);
    set_num("HDRToneMap:BaseHeadroom", 0.0);
    set_num("HDRToneMap:AlternateHeadroom", 2.287109);

    // Build the ChannelMetadata[0] struct directly (path-based creation of a
    // struct-in-array failed above), then set the whole array in one call.
    let num = |v: f64| -> CFRetained<CFNumber> {
        unsafe { CFNumber::new(None, CFNumberType::Float64Type, &v as *const f64 as *const c_void) }
            .unwrap()
    };
    let sub_tag = |name: &str, v: f64| -> CFRetained<CGImageMetadataTag> {
        let n = CFString::from_str(name);
        let val = num(v);
        unsafe { CGImageMetadataTag::new(&ns, Some(&prefix), &n, CGImageMetadataType::Default, val.as_ref()) }
            .unwrap()
    };
    let fields: [(&str, f64); 5] = [
        ("GainMapMin", -0.001963),
        ("GainMapMax", 2.287109),
        ("Gamma", 0.825684),
        ("BaseOffset", 0.00001),
        ("AlternateOffset", 0.00001),
    ];
    let keys: Vec<CFRetained<CFString>> = fields.iter().map(|(k, _)| CFString::from_str(k)).collect();
    let vals: Vec<CFRetained<CGImageMetadataTag>> = fields.iter().map(|(k, v)| sub_tag(k, *v)).collect();
    let key_refs: Vec<&CFString> = keys.iter().map(|k| k.as_ref()).collect();
    let val_refs: Vec<&CGImageMetadataTag> = vals.iter().map(|v| v.as_ref()).collect();
    let struct_dict = CFDictionary::<CFString, CGImageMetadataTag>::from_slices(&key_refs, &val_refs);
    let struct_name = CFString::from_str("[0]");
    let struct_tag = unsafe {
        CGImageMetadataTag::new(
            &ns,
            Some(&prefix),
            &struct_name,
            CGImageMetadataType::Structure,
            struct_dict.as_opaque().as_ref(),
        )
    }
    .unwrap();
    let arr = CFArray::<CGImageMetadataTag>::from_objects(&[struct_tag.as_ref()]);
    let arr_path = CFString::from_str("HDRToneMap:ChannelMetadata");
    let ok = unsafe { md.set_value_with_path(None, &arr_path, arr.as_opaque().as_ref()) };
    println!("set ChannelMetadata array -> {ok}");

    let bpath = CFString::from_str("HDRToneMap:BaseColorIsWorkingColor");
    let btrue = objc2_core_foundation::CFBoolean::new(true);
    let ok = unsafe { md.set_value_with_path(None, &bpath, btrue.as_ref()) };
    println!("set BaseColorIsWorkingColor -> {ok}");

    let cf: &CFType = md.as_ref();
    println!("\nfinal tree:\n{cf:?}");
}
