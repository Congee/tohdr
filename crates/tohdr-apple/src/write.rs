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
    CFArray, CFBoolean, CFData, CFDictionary, CFMutableData, CFNumber, CFNumberType, CFRetained,
    CFString, CFType,
};
use objc2_core_graphics::{
    CGBitmapInfo, CGColorRenderingIntent, CGColorSpace, CGDataProvider, CGImage,
    CGImageAlphaInfo, CGImageByteOrderInfo, CGImageComponentInfo,
    kCGColorSpaceExtendedLinearSRGB, kCGColorSpaceGenericGrayGamma2_2, kCGColorSpaceSRGB,
};
use objc2_image_io::{
    kCGImageAuxiliaryDataInfoData, kCGImageAuxiliaryDataInfoDataDescription,
    kCGImageAuxiliaryDataInfoMetadata, kCGImageAuxiliaryDataTypeHDRGainMap,
    kCGImageAuxiliaryDataTypeISOGainMap, kCGImageDestinationEncodeRequest,
    kCGImageDestinationEncodeToISOGainmap, kCGImageDestinationLossyCompressionQuality,
    kCGImagePropertyExifDictionary, kCGImagePropertyGPSDictionary, kCGImagePropertyTIFFDictionary,
    CGImageDestination, CGImageMetadataTag, CGImageMetadataType, CGImageSource,
    CGMutableImageMetadata,
};
use tohdr_core::{EncodeOptions, GainMapMeta, GainPlane, HdrRgb, Rgb};

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
    // 4 components, not 3. CoreGraphics has no valid alpha-less 96 bpp RGB
    // float layout; `CGImageCreate` accepts the arithmetic without complaint
    // and then misreads the buffer. Measured on the same source: the 96 bpp
    // variant decodes back to a base mean of 66.8/255 where every 128 bpp
    // variant gives 161.4/255, and its file is 3x larger for identical
    // content. The read side (`read::render`) already used 128 bpp.
    let n = (hdr.width * hdr.height) as usize;
    let mut samples: Vec<f32> = Vec::with_capacity(n * 4);
    for i in 0..n {
        samples.extend_from_slice(&hdr.data[i * 3..i * 3 + 3]);
        samples.push(1.0); // opaque; the skip channel is ignored on read
    }
    let bytes: Vec<u8> = samples.iter().flat_map(|v| v.to_ne_bytes()).collect();
    let data = CFData::from_bytes(&bytes);
    let provider = CGDataProvider::with_cf_data(Some(&data))
        .ok_or(Error::NullFromFramework("CGDataProviderCreateWithCFData"))?;
    let cs = unsafe { CGColorSpace::with_name(Some(kCGColorSpaceExtendedLinearSRGB)) }
        .ok_or(Error::NullFromFramework("CGColorSpaceCreateWithName"))?;

    // Float samples, little-endian, 4th channel present but ignored. Spelled
    // through the component and byte-order enums because the flat
    // `CGBitmapInfo` aliases for those two bits are deprecated.
    let bitmap = CGBitmapInfo(
        CGImageComponentInfo::Float.0
            | CGImageByteOrderInfo::Order32Little.0
            | CGImageAlphaInfo::NoneSkipLast.0,
    );

    unsafe {
        CGImage::new(
            hdr.width as usize,
            hdr.height as usize,
            32,
            128,
            hdr.width as usize * 16,
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

/// Encode one plane as a single-image HEIC, i.e. the hardware equivalent of
/// `hpvca::encode_rgb`.
///
/// This exists so the muxing half of Engine B can keep our own
/// [`tohdr_heif`](../../tohdr_heif/index.html) muxer while the *encode* half
/// runs on the platform's media block. On Apple Silicon ImageIO routes HEVC
/// through VideoToolbox, which is the same fixed-function encoder Engine A
/// reaches — and profiling established that the encoder, not our muxer, is
/// where Engine B's entire deficit lives (see `docs/engine-comparison.md`).
///
/// The output is hpvca's contract exactly: one coded image item, no `grid`, so
/// [`tohdr_heif::HeifFile::coded_image`] can pull the HEVC and `hvcC` straight
/// back out with no re-encode.
pub fn encode_plane_heic_rgb(rgb: &Rgb, quality: u8) -> Result<Vec<u8>> {
    let image = cg_image_from_sdr(rgb)?;
    finalize_single_image(&image, quality)
}

/// The gain-plane counterpart of [`encode_plane_heic_rgb`]. Single-channel, to
/// match ISO 21496-1 and Apple's own `L008` planes.
pub fn encode_plane_heic_gray(gain: &GainPlane, quality: u8) -> Result<Vec<u8>> {
    let data = CFData::from_bytes(&gain.data);
    let provider = CGDataProvider::with_cf_data(Some(&data))
        .ok_or(Error::NullFromFramework("CGDataProviderCreateWithCFData"))?;
    let cs = unsafe { CGColorSpace::with_name(Some(kCGColorSpaceGenericGrayGamma2_2)) }
        .ok_or(Error::NullFromFramework("CGColorSpaceCreateWithName (gray)"))?;
    let image = unsafe {
        CGImage::new(
            gain.width as usize,
            gain.height as usize,
            8,
            8,
            gain.width as usize,
            Some(&cs),
            CGBitmapInfo(CGImageAlphaInfo::None.0),
            Some(&provider),
            std::ptr::null(),
            false,
            CGColorRenderingIntent::RenderingIntentDefault,
        )
    }
    .ok_or(Error::NullFromFramework("CGImageCreate (gray)"))?;
    finalize_single_image(&image, quality)
}

/// Shared tail of the two plane encoders: a HEIC destination holding exactly one
/// image, no gain-map request and no auxiliary data.
fn finalize_single_image(image: &CGImage, quality: u8) -> Result<Vec<u8>> {
    let out = CFMutableData::new(None, 0).ok_or(Error::NullFromFramework("CFDataCreateMutable"))?;
    let uti = CFString::from_str(HEIC_UTI);
    let dest = unsafe { CGImageDestination::with_data(&out, &uti, 1, None) }
        .ok_or(Error::NullFromFramework("CGImageDestinationCreateWithData"))?;
    let q = cf_num_f64((quality.clamp(1, 100) as f64) / 100.0);
    let opts = cf_dict(&[(
        unsafe { kCGImageDestinationLossyCompressionQuality },
        q.as_ref(),
    )]);
    unsafe { dest.add_image(image, Some(&opts)) };
    if !unsafe { dest.finalize() } {
        return Err(Error::FinalizeFailed);
    }
    Ok(out.to_vec())
}

/// `L008`: one 8-bit luminance channel, the `PixelFormat` Apple reports for
/// every gain plane measured (`IMG_4913.HEIC`, `DSC07752.heic`, and ImageIO's
/// own output).
const PIXEL_FORMAT_L008: i64 = 1_278_226_488;

/// Build the `CGImageMetadata` holding the ISO 21496-1 parameters, in the
/// `HDRToneMap` namespace ImageIO reads them back from.
///
/// Shape reverse-engineered from `IMG_4913.HEIC` via `examples/probe_meta`:
/// three scalars at the top level plus a `ChannelMetadata` array whose single
/// element is a *structure* tag carrying the five per-channel parameters.
fn tone_map_metadata(meta: &GainMapMeta) -> Result<CFRetained<CGMutableImageMetadata>> {
    let md = unsafe { CGMutableImageMetadata::new() };
    let ns = CFString::from_str("http://ns.apple.com/HDRToneMap/1.0/");
    let prefix = CFString::from_str("HDRToneMap");
    unsafe { md.register_namespace_for_prefix(&ns, &prefix, std::ptr::null_mut()) };

    let set = |path: &str, v: f64| {
        let p = CFString::from_str(path);
        let n = cf_num_f64(v);
        unsafe { md.set_value_with_path(None, &p, n.as_ref()) };
    };
    set("HDRToneMap:Version", 1.0);
    set("HDRToneMap:BaseHeadroom", meta.base_headroom as f64);
    set("HDRToneMap:AlternateHeadroom", meta.alt_headroom as f64);

    let fields: [(&str, f64); 5] = [
        ("GainMapMin", meta.min_log2[0] as f64),
        ("GainMapMax", meta.max_log2[0] as f64),
        ("Gamma", meta.gamma[0] as f64),
        ("BaseOffset", meta.base_offset[0] as f64),
        ("AlternateOffset", meta.alt_offset[0] as f64),
    ];
    let keys: Vec<CFRetained<CFString>> =
        fields.iter().map(|(k, _)| CFString::from_str(k)).collect();
    let vals: Vec<CFRetained<CGImageMetadataTag>> = fields
        .iter()
        .map(|(k, v)| {
            let name = CFString::from_str(k);
            let num = cf_num_f64(*v);
            unsafe {
                CGImageMetadataTag::new(
                    &ns,
                    Some(&prefix),
                    &name,
                    CGImageMetadataType::Default,
                    num.as_ref(),
                )
            }
            .ok_or(Error::NullFromFramework("CGImageMetadataTagCreate"))
        })
        .collect::<Result<_>>()?;

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
    .ok_or(Error::NullFromFramework("CGImageMetadataTagCreate (struct)"))?;

    let arr = CFArray::<CGImageMetadataTag>::from_objects(&[struct_tag.as_ref()]);
    let arr_path = CFString::from_str("HDRToneMap:ChannelMetadata");
    unsafe { md.set_value_with_path(None, &arr_path, arr.as_opaque().as_ref()) };

    let bpath = CFString::from_str("HDRToneMap:BaseColorIsWorkingColor");
    let b = CFBoolean::new(meta.use_base_color_space);
    unsafe { md.set_value_with_path(None, &bpath, b.as_ref()) };

    Ok(md)
}

/// Build the `CGImageMetadata` carrying Apple's XMP gain-map headroom, so
/// Engine A states the headroom in the same places Engine B does.
///
/// `HDRGainMapHeadroom` is linear, not stops -- see `tohdr_core::xmp`, which
/// owns that conversion and the reasoning behind it.
fn gain_map_xmp_metadata(meta: &GainMapMeta) -> Result<CFRetained<CGMutableImageMetadata>> {
    let md = unsafe { CGMutableImageMetadata::new() };
    let ns = CFString::from_str(tohdr_core::xmp::HDR_GAIN_MAP_NS);
    let prefix = CFString::from_str("HDRGainMap");
    unsafe { md.register_namespace_for_prefix(&ns, &prefix, std::ptr::null_mut()) };

    let version = cf_num_f64(tohdr_core::xmp::HDR_GAIN_MAP_VERSION as f64);
    let vpath = CFString::from_str("HDRGainMap:HDRGainMapVersion");
    unsafe { md.set_value_with_path(None, &vpath, version.as_ref()) };

    let headroom = cf_num_f64(meta.alt_headroom.exp2() as f64);
    let hpath = CFString::from_str("HDRGainMap:HDRGainMapHeadroom");
    unsafe { md.set_value_with_path(None, &hpath, headroom.as_ref()) };

    Ok(md)
}

/// Turn a raw Exif block into the property dictionaries ImageIO writes from.
///
/// ImageIO's destination API takes no raw Exif: metadata goes in as
/// `kCGImageProperty*Dictionary` entries, keyed by CF strings. Rather than carry
/// a table mapping every TIFF tag number onto its key — several hundred lines
/// that would silently drop each tag Apple adds — this hands the block to
/// ImageIO's *reader* and passes what comes back straight to its writer. The
/// wrapper exists because `CGImageSource` will not read a TIFF with no pixels in
/// it; see [`tohdr_core::exif::wrap_in_jpeg`].
///
/// Returns the pairs to merge into the image's options dictionary, empty if the
/// block yielded nothing. Never an error: losing metadata must not fail an
/// encode that would otherwise succeed.
fn exif_property_pairs(block: &[u8]) -> Vec<(&'static CFString, CFRetained<CFType>)> {
    let Some(carrier) = tohdr_core::exif::wrap_in_jpeg(block) else {
        return Vec::new();
    };
    let data = CFData::from_bytes(&carrier);
    let Some(isrc) = (unsafe { CGImageSource::with_data(&data, None) }) else {
        return Vec::new();
    };
    let Some(props) = (unsafe { isrc.properties_at_index(0, None) }) else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for key in [
        unsafe { kCGImagePropertyExifDictionary },
        unsafe { kCGImagePropertyTIFFDictionary },
        unsafe { kCGImagePropertyGPSDictionary },
    ] {
        let ptr = unsafe { props.value(key as *const CFString as *const c_void) };
        if ptr.is_null() {
            continue;
        }
        // Retained because `props` is dropped at the end of this function while
        // the values have to outlive it, up to the `add_image` call.
        let value = unsafe { &*(ptr as *const CFType) };
        out.push((key, unsafe {
            CFRetained::retain(NonNull::from(value))
        }));
    }
    out
}

/// Attach a gain plane *we* derived to an SDR base, through ImageIO.
///
/// This is the path the engine comparison uses, so both engines encode the
/// same derived inputs — otherwise a benchmark would be measuring two
/// different derivations as much as two different containers.
pub fn encode_parts(
    base: &Rgb,
    gain: &GainPlane,
    meta: &GainMapMeta,
    opts: &EncodeOptions,
) -> Result<Vec<u8>> {
    let image = cg_image_from_sdr(base)?;
    let out = CFMutableData::new(None, 0).ok_or(Error::NullFromFramework("CFDataCreateMutable"))?;
    let uti = CFString::from_str(HEIC_UTI);
    let dest = unsafe { CGImageDestination::with_data(&out, &uti, 1, None) }
        .ok_or(Error::NullFromFramework("CGImageDestinationCreateWithData"))?;

    let q = cf_num_f64((opts.base_quality.clamp(1, 100) as f64) / 100.0);
    let mut pairs: Vec<(&CFString, &CFType)> = vec![(
        unsafe { kCGImageDestinationLossyCompressionQuality },
        q.as_ref(),
    )];
    // Held in a binding so the retained dictionaries outlive `add_image`.
    let exif = opts.exif.map(exif_property_pairs).unwrap_or_default();
    pairs.extend(exif.iter().map(|(k, v)| (*k, v.as_ref())));
    let img_opts = cf_dict(&pairs);
    if opts.flavor.writes_apple() {
        let xmp = gain_map_xmp_metadata(meta)?;
        unsafe { dest.add_image_and_metadata(&image, Some(&xmp), Some(&img_opts)) };
    } else {
        unsafe { dest.add_image(&image, Some(&img_opts)) };
    }

    // The gain plane, described the way ImageIO describes one when reading:
    // tightly packed 8-bit luminance, `BytesPerRow` = width.
    let plane = CFData::from_bytes(&gain.data);
    let w = CFNumber::new_i64(gain.width as i64);
    let h = CFNumber::new_i64(gain.height as i64);
    let bpr = CFNumber::new_i64(gain.width as i64);
    let fmt = CFNumber::new_i64(PIXEL_FORMAT_L008);
    let desc = cf_dict(&[
        (&CFString::from_str("Width"), w.as_ref()),
        (&CFString::from_str("Height"), h.as_ref()),
        (&CFString::from_str("BytesPerRow"), bpr.as_ref()),
        (&CFString::from_str("PixelFormat"), fmt.as_ref()),
    ]);

    let md = tone_map_metadata(meta)?;
    let aux = cf_dict(&[
        (
            unsafe { kCGImageAuxiliaryDataInfoData },
            plane.as_ref(),
        ),
        (
            unsafe { kCGImageAuxiliaryDataInfoDataDescription },
            desc.as_opaque().as_ref(),
        ),
        (
            unsafe { kCGImageAuxiliaryDataInfoMetadata },
            md.as_ref(),
        ),
    ]);

    if opts.flavor.writes_iso() {
        let k: &CFString = unsafe { kCGImageAuxiliaryDataTypeISOGainMap };
        unsafe { dest.add_auxiliary_data_info(k, &aux) };
    }
    if opts.flavor.writes_apple() {
        let k: &CFString = unsafe { kCGImageAuxiliaryDataTypeHDRGainMap };
        unsafe { dest.add_auxiliary_data_info(k, &aux) };
    }

    if !unsafe { dest.finalize() } {
        return Err(Error::FinalizeFailed);
    }
    Ok(out.to_vec())
}
