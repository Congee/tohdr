//! Can ImageIO be handed a bare Exif block, or must Engine A map every tag?
//!
//! Engine B carries Exif for free: our muxer already writes the `Exif` item, so
//! the block passes through as opaque bytes. Engine A has no such door. ImageIO
//! authors the whole file, and a `CGImage` built from raw pixels carries no
//! metadata for it to copy — so the only way in is
//! `CGImageDestinationAddImage`'s properties dictionary, keyed by
//! `kCGImagePropertyExifDictionary` and friends. Building those by hand means
//! mapping ~55 tags from TIFF numbers onto CF string keys, per tag, with a
//! type conversion each: a large amount of code that ages badly.
//!
//! The shortcut worth testing first: an Exif block *is* a TIFF structure, and
//! ImageIO parses TIFF. If `CGImageSource` will read properties out of a block
//! that has no pixel data, then Engine A's Exif support is "parse with ImageIO,
//! hand the dictionaries straight back to ImageIO" — no tag table at all.
//!
//! Run: `cargo run --release --example probe_exif_props -p tohdr-apple -- <src.heic>`

use std::ffi::c_void;

use objc2_core_foundation::{CFData, CFDictionary, CFRetained, CFString, CFType};
use objc2_image_io::{
    kCGImagePropertyExifDictionary, kCGImagePropertyGPSDictionary,
    kCGImagePropertyTIFFDictionary, CGImageSource,
};

fn get<'a>(dict: &'a CFDictionary, key: &CFString) -> Option<&'a CFType> {
    let ptr = unsafe { dict.value(key as *const CFString as *const c_void) };
    (!ptr.is_null()).then(|| unsafe { &*(ptr as *const CFType) })
}

/// Every key in a `CFDictionary`, as strings, sorted.
fn keys(dict: &CFDictionary) -> Vec<String> {
    let n = dict.count() as usize;
    let mut raw: Vec<*const c_void> = vec![std::ptr::null(); n];
    unsafe { dict.keys_and_values(raw.as_mut_ptr(), std::ptr::null_mut()) };
    let mut out: Vec<String> = raw
        .iter()
        .filter(|p| !p.is_null())
        .filter_map(|p| unsafe { &*(*p as *const CFType) }.downcast_ref::<CFString>())
        .map(|s| s.to_string())
        .collect();
    out.sort();
    out
}

fn report(label: &str, props: &CFDictionary) {
    println!("  {label}: {} top-level keys", props.count());
    for (name, key) in [
        ("Exif", unsafe { kCGImagePropertyExifDictionary }),
        ("TIFF", unsafe { kCGImagePropertyTIFFDictionary }),
        ("GPS", unsafe { kCGImagePropertyGPSDictionary }),
    ] {
        match get(props, key).and_then(|v| v.downcast_ref::<CFDictionary>()) {
            Some(d) => {
                let k = keys(d);
                println!("    {name}: {} entries", k.len());
                println!("      {}", k.join(", "));
            }
            None => println!("    {name}: absent"),
        }
    }
}

fn source_properties(bytes: &[u8]) -> Option<CFRetained<CFDictionary>> {
    let data = CFData::from_bytes(bytes);
    let isrc = unsafe { CGImageSource::with_data(&data, None) }?;
    println!(
        "  CGImageSource: status={:?} count={}",
        unsafe { isrc.status() },
        unsafe { isrc.count() }
    );
    unsafe { isrc.properties_at_index(0, None) }
}

fn main() {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: probe_exif_props <src.heic>");
        std::process::exit(2);
    };
    let bytes = std::fs::read(&path).expect("read source");

    println!("== the whole source file, as a control ==");
    match source_properties(&bytes) {
        Some(p) => report("file", &p),
        None => println!("  no properties"),
    }

    let file = tohdr_heif::HeifFile::parse(&bytes).expect("parse heif");
    let Some(item) = file.items().iter().find(|i| i.item_type == *b"Exif") else {
        println!("\n== source has no Exif item, nothing to probe ==");
        return;
    };
    let data = file.item_data(item.id).expect("exif item payload");
    let skip = u32::from_be_bytes([data[0], data[1], data[2], data[3]]) as usize;
    let block = &data[4 + skip..];
    println!(
        "\n== the bare Exif block ({} bytes, byte order {:?}) ==",
        block.len(),
        std::str::from_utf8(&block[0..2]).unwrap_or("??")
    );
    match source_properties(block) {
        Some(p) => report("block", &p),
        None => println!("  no properties — ImageIO will not read a pixel-less TIFF"),
    }
}
