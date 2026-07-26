//! Can ImageIO carry a source's XMP through `CGImageMetadata`?
//!
//! Engine A cannot write an XMP item directly — ImageIO authors the container and
//! takes XMP only as a `CGImageMetadata`. So carrying a photographer's keywords
//! depends on two ImageIO behaviours this measures rather than assumes:
//!
//! 1. does `CGImageMetadataCreateFromXMPData` parse a real packet, and does the
//!    mutable copy still hold its tags?
//! 2. does `CGImageDestinationAddImageAndMetadata` then *write* those tags into a
//!    HEIC, or only the ones in namespaces it recognizes?
//!
//! ```text
//! cargo run --release -p tohdr-apple --example probe_xmp_metadata
//! ```

use objc2_core_foundation::{CFData, CFMutableData, CFString};
use objc2_image_io::{
    CGImageDestination, CGImageMetadata, CGImageSource, CGMutableImageMetadata,
};

const PACKET: &str = r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/" x:xmptk="Adobe XMP Core 9.1">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:dc="http://purl.org/dc/elements/1.1/"
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmp:Rating="4">
   <dc:title><rdf:Alt><rdf:li xml:lang="x-default">Evening</rdf:li></rdf:Alt></dc:title>
   <dc:subject><rdf:Bag><rdf:li>sunset</rdf:li></rdf:Bag></dc:subject>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>
<?xpacket end="w"?>"#;

fn main() {
    // With a path argument, probe that file's raw packet instead of the built-in
    // one: a packet a real writer produced is the case that matters, and it is
    // not necessarily one ImageIO accepts.
    let packet = match std::env::args().nth(1) {
        Some(p) => {
            let bytes = std::fs::read(&p).expect("read packet");
            println!("packet from {p}: {} bytes", bytes.len());
            println!("  head: {:?}", String::from_utf8_lossy(&bytes[..bytes.len().min(60)]));
            let tail = &bytes[bytes.len().saturating_sub(40)..];
            println!("  tail: {:?}", String::from_utf8_lossy(tail));
            bytes
        }
        None => PACKET.as_bytes().to_vec(),
    };
    // Does going through the *file* rather than the packet dodge the parser? If
    // ImageIO reads the source itself, no extraction or rewriting is needed.
    if let Some(p) = std::env::args().nth(2) {
        let bytes = std::fs::read(&p).expect("read image");
        let d = CFData::from_bytes(&bytes);
        match unsafe { CGImageSource::with_data(&d, None) }
            .and_then(|s| unsafe { s.metadata_at_index(0, None) })
        {
            Some(md) => {
                println!("\nCGImageSourceCopyMetadataAtIndex on {p}: ok");
                for path in ["dc:subject", "dc:title", "xmp:Rating"] {
                    let q = CFString::from_str(path);
                    println!(
                        "  {path:<12} present: {}",
                        unsafe { md.tag_with_path(None, &q) }.is_some()
                    );
                }
            }
            None => println!("\nCGImageSourceCopyMetadataAtIndex on {p}: NULL"),
        }
    }

    let data = CFData::from_bytes(&packet);
    let direct = unsafe { CGImageMetadata::from_xmp_data(&data) };
    println!(
        "CGImageMetadataCreateFromXMPData: {}",
        if direct.is_some() { "ok" } else { "NULL" }
    );

    // The carrier route: the same packet inside a 1x1 JPEG's XMP APP1, read by
    // ImageIO's file-level parser instead of its packet parser.
    let carried = tohdr_core::exif::wrap_xmp_in_jpeg(&packet)
        .map(|j| CFData::from_bytes(&j))
        .and_then(|d| unsafe { CGImageSource::with_data(&d, None) })
        .and_then(|s| unsafe { s.metadata_at_index(0, None) });
    println!(
        "same packet via a JPEG carrier:   {}",
        if carried.is_some() { "ok" } else { "NULL" }
    );
    let Some(parsed) = direct.or(carried) else {
        println!("neither route parses this packet");
        return;
    };

    for path in ["dc:subject", "dc:title", "xmp:Rating"] {
        let p = CFString::from_str(path);
        let tag = unsafe { parsed.tag_with_path(None, &p) };
        println!("  {path:<12} tag present: {}", tag.is_some());
    }

    let Some(copy) = (unsafe { CGMutableImageMetadata::new_copy(&parsed) }) else {
        println!("CGImageMetadataCreateMutableCopy: NULL");
        return;
    };
    println!("CGImageMetadataCreateMutableCopy: ok");
    for path in ["dc:subject", "dc:title", "xmp:Rating"] {
        let p = CFString::from_str(path);
        let tag = unsafe { copy.tag_with_path(None, &p) };
        println!("  {path:<12} survives the copy: {}", tag.is_some());
    }

    // Serialize the copy back to a packet: if the tags are here, the loss is in
    // the *destination*, not in the metadata object.
    match unsafe { copy.xmp_data(None) } {
        Some(out) => {
            let bytes = out.to_vec();
            let s = String::from_utf8_lossy(&bytes);
            println!("\nCGImageMetadataCreateXMPData: {} bytes", bytes.len());
            for needle in ["dc:subject", "dc:title", "xmp:Rating", "sunset", "Evening"] {
                println!("  contains {needle:<12}: {}", s.contains(needle));
            }
        }
        None => println!("\nCGImageMetadataCreateXMPData: NULL"),
    }

    // The decisive question: does the *destination* write those tags into a HEIC?
    // Everything above lives inside the metadata object, which is not where the
    // tags were going missing.
    let base = tohdr_core::Rgb {
        width: 8,
        height: 8,
        bits: 8,
        data: vec![128u16; 8 * 8 * 3],
    };
    let image = tohdr_apple::cg_image_for_probe(&base, tohdr_core::Primaries::Bt709).expect("CGImage");
    let out = CFMutableData::new(None, 0).expect("CFMutableData");
    let uti = CFString::from_str("public.heic");
    let dest = unsafe { CGImageDestination::with_data(&out, &uti, 1, None) }.expect("destination");
    unsafe { dest.add_image_and_metadata(&image, Some(&copy), None) };
    if !unsafe { dest.finalize() } {
        println!("\nCGImageDestinationFinalize failed");
        return;
    }
    let written = out.to_vec();
    println!(
        "\nadd_image_and_metadata -> {} byte HEIC; reading its XMP back:",
        written.len()
    );
    let d = CFData::from_bytes(&written);
    let Some(src) = (unsafe { CGImageSource::with_data(&d, None) }) else {
        println!("  unreadable");
        return;
    };
    match unsafe { src.metadata_at_index(0, None) }
        .and_then(|md| unsafe { md.xmp_data(None) })
        .map(|d| d.to_vec())
    {
        Some(bytes) => {
            let s = String::from_utf8_lossy(&bytes);
            println!("  {} bytes of XMP in the file", bytes.len());
            for needle in ["dc:subject", "dc:title", "xmp:Rating", "sunset", "Evening"] {
                println!("    contains {needle:<12}: {}", s.contains(needle));
            }
        }
        None => println!("  no XMP in the written file at all"),
    }
}
