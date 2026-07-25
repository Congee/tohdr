//! Scratch probe (not part of the public API): what is in a Lightroom Classic
//! "HDR Output" TIFF, and how much of it does ImageIO see?
//!
//! LrC 15.4.1 with *HDR Output* checked and Color Space "HDR sRGB (Rec. 709)"
//! writes a two-image TIFF: an sRGB SDR base in IFD0, and a full-resolution
//! 3-channel 16-bit gain map in a SubIFD whose `PhotometricInterpretation` is
//! 52553 and which carries a 145-byte tag 52557. Those 145 bytes look like
//! ISO 21496-1 clause C.2.2 behind a 4-byte zero prefix — this checks that
//! claim with our own parser instead of by eye, then asks ImageIO whether it
//! surfaces any of it.
//!
//! Usage: probe_lr_tiff <payload.bin> <lr.tif>
use objc2_core_foundation::{CFRetained, CFString, CFURL, CFURLPathStyle};
use objc2_image_io::{
    kCGImageAuxiliaryDataTypeHDRGainMap, kCGImageAuxiliaryDataTypeISOGainMap, CGImageSource,
};

fn main() {
    let mut args = std::env::args().skip(1);
    let payload = args.next().expect("usage: probe_lr_tiff <payload.bin> <lr.tif>");
    let tiff = args.next().expect("usage: probe_lr_tiff <payload.bin> <lr.tif>");

    let bytes = std::fs::read(&payload).expect("read payload");
    println!("== tohdr_core::iso21496::parse on {} bytes ==", bytes.len());
    match tohdr_core::iso21496::parse(&bytes) {
        Err(e) => println!("  PARSE FAILED: {e}"),
        Ok(m) => {
            println!("  base_headroom  = {:.6} stops  (linear {:.4})", m.base_headroom, m.base_headroom.exp2());
            println!("  alt_headroom   = {:.6} stops  (linear {:.4})", m.alt_headroom, m.alt_headroom.exp2());
            println!("  use_base_color_space = {}", m.use_base_color_space);
            for c in 0..3 {
                println!(
                    "  ch{c}: min_log2={:+.6} max_log2={:+.6} gamma={:.6} base_off={:.6} alt_off={:.6}",
                    m.min_log2[c], m.max_log2[c], m.gamma[c], m.base_offset[c], m.alt_offset[c]
                );
            }
            // Where does an encoded sample of "no change" land, per channel?
            for c in 0..3 {
                let t = (-m.min_log2[c]) / (m.max_log2[c] - m.min_log2[c]);
                println!("  ch{c}: unity gain at encoded {:.4} (= {:.0}/65535)", t.powf(m.gamma[c]), t.powf(m.gamma[c]) * 65535.0);
            }
            // Round-trip through our own serializer.
            let re = tohdr_core::iso21496::serialize(&m);
            println!(
                "  re-serialize: {} bytes, byte-identical to Adobe's payload: {}",
                re.len(),
                re == bytes
            );
            if re != bytes {
                let diff = re.iter().zip(&bytes).filter(|(a, b)| a != b).count();
                println!("  {diff} of {} bytes differ; same length, so check it is only", bytes.len());
                println!("  rational quantization by re-parsing our own bytes:");
                match tohdr_core::iso21496::parse(&re) {
                    Err(e) => println!("    RE-PARSE FAILED: {e}"),
                    Ok(m2) => {
                        let worst = [
                            ("base_headroom", (m2.base_headroom - m.base_headroom).abs()),
                            ("alt_headroom", (m2.alt_headroom - m.alt_headroom).abs()),
                            ("min_log2", (0..3).map(|c| (m2.min_log2[c] - m.min_log2[c]).abs()).fold(0.0f32, f32::max)),
                            ("max_log2", (0..3).map(|c| (m2.max_log2[c] - m.max_log2[c]).abs()).fold(0.0f32, f32::max)),
                            ("gamma", (0..3).map(|c| (m2.gamma[c] - m.gamma[c]).abs()).fold(0.0f32, f32::max)),
                            ("base_offset", (0..3).map(|c| (m2.base_offset[c] - m.base_offset[c]).abs()).fold(0.0f32, f32::max)),
                            ("alt_offset", (0..3).map(|c| (m2.alt_offset[c] - m.alt_offset[c]).abs()).fold(0.0f32, f32::max)),
                        ];
                        for (name, d) in worst {
                            println!("    max |delta| {name:<14} = {d:.3e}");
                        }
                    }
                }
            }
        }
    }

    println!("\n== ImageIO on {tiff} ==");
    let cfpath = CFString::from_str(&tiff);
    let url = CFURL::with_file_system_path(None, Some(&cfpath), CFURLPathStyle::CFURLPOSIXPathStyle, false).unwrap();
    let isrc: CFRetained<CGImageSource> = unsafe { CGImageSource::with_url(&url, None) }.unwrap();
    let n = unsafe { isrc.count() };
    println!("  CGImageSourceGetCount = {n}");
    for i in 0..n {
        let props = unsafe { isrc.properties_at_index(i, None) };
        match props {
            None => println!("  [{i}] properties: <none>"),
            Some(d) => println!("  [{i}] properties: {d:?}"),
        }
        for (label, key) in [
            ("HDRGainMap", unsafe { kCGImageAuxiliaryDataTypeHDRGainMap }),
            ("ISOGainMap", unsafe { kCGImageAuxiliaryDataTypeISOGainMap }),
        ] {
            let aux = unsafe { isrc.auxiliary_data_info_at_index(i, key) };
            println!("  [{i}] aux {label}: {}", if aux.is_some() { "PRESENT" } else { "absent" });
            if let Some(d) = aux {
                let _ = &d as &dyn std::fmt::Debug;
                println!("        {d:?}");
            }
        }
    }
}
