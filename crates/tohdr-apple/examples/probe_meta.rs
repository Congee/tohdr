//! Scratch probe (not part of the public API): dump the raw CFType behind the
//! `kCGImageAuxiliaryDataInfoMetadata` key of the ISO gain-map aux dict, so we
//! can find out whether it holds a `CGImageMetadataRef` and, if so, what tags
//! it carries. Safe to delete; exists only to inform the `inspect()` impl.
use std::ffi::c_void;

use objc2_core_foundation::{CFDictionary, CFRetained, CFString, CFType, CFURL, CFURLPathStyle};
use objc2_image_io::{
    kCGImageAuxiliaryDataInfoMetadata, kCGImageAuxiliaryDataTypeISOGainMap, CGImageMetadata,
    CGImageSource,
};

unsafe fn cf_ref<'a>(ptr: *const c_void) -> &'a CFType {
    unsafe { &*(ptr as *const CFType) }
}

fn main() {
    let path = "~/Downloads/IMG_4913.HEIC";
    let cfpath = CFString::from_str(path);
    let url = CFURL::with_file_system_path(
        None,
        Some(&cfpath),
        CFURLPathStyle::CFURLPOSIXPathStyle,
        false,
    )
    .unwrap();
    let isrc: CFRetained<CGImageSource> = unsafe { CGImageSource::with_url(&url, None) }.unwrap();
    let iso_key: &'static CFString = unsafe { kCGImageAuxiliaryDataTypeISOGainMap };
    let dict = unsafe { isrc.auxiliary_data_info_at_index(0, iso_key) }.unwrap();

    let meta_key: &'static CFString = unsafe { kCGImageAuxiliaryDataInfoMetadata };
    let ptr = unsafe { dict.value(meta_key as *const CFString as *const c_void) };
    if ptr.is_null() {
        println!("Metadata key: <missing>");
        return;
    }
    let cf = unsafe { cf_ref(ptr) };
    println!("Metadata native description: {cf:?}");
    println!("Metadata CFTypeID matches CGImageMetadata: {}", cf.downcast_ref::<CGImageMetadata>().is_some());
    if let Some(md) = cf.downcast_ref::<CGImageMetadata>() {
        let tags = unsafe { md.tags() };
        match tags {
            None => println!("  tags() -> None"),
            Some(tags) => {
                println!("  tags() -> {} tags", tags.count());
                for i in 0..tags.count() {
                    let t = unsafe { tags.value_at_index(i) };
                    let t = unsafe { &*(t as *const objc2_image_io::CGImageMetadataTag) };
                    let ns = unsafe { t.namespace() };
                    let name = unsafe { t.name() };
                    let val = unsafe { t.value() };
                    println!(
                        "    ns={:?} name={:?} value={:?}",
                        ns.map(|s| s.to_string()),
                        name.map(|s| s.to_string()),
                        val
                    );
                }
            }
        }
    }

    // Also dump DataDescription fully and top-level dict native description
    // for good measure, and whether a ColorSpace key round-trips.
    println!("\nfull ISO aux dict native description:\n{dict:?}");

    let _ = CFDictionary::<CFType, CFType>::from_slices(&[], &[]); // keep CFDictionary import used
}
