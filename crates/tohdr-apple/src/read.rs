//! Asking ImageIO what it sees in a file.
//!
//! This is the oracle half of Engine A. Every value here comes from Apple's own
//! decoder rather than our parser, so a bug shared between `tohdr-heif`'s reader
//! and its writer cannot hide behind it.
//!
//! # Where the numbers live
//!
//! ImageIO does not hand back the raw ISO 21496-1 struct. It parses the `tmap`
//! payload and re-exposes it as a `CGImageMetadata` under the aux dictionary's
//! `kCGImageAuxiliaryDataInfoMetadata` key, with tags in the
//! `http://ns.apple.com/HDRToneMap/1.0/` namespace. Verified against
//! `IMG_4913.HEIC`, whose tags match `assets/fixtures/img4913_reference.json`
//! field for field — two independent decoders agreeing on the same bytes.
//!
//! The per-channel parameters (`GainMapMin`, `GainMapMax`, `Gamma`, and the two
//! offsets) are nested one level down, inside a `ChannelMetadata` array whose
//! elements are themselves tags holding a dictionary. Apple writes one element
//! for a monochrome map.

use std::ffi::c_void;
use std::path::Path;

use objc2_core_foundation::{
    CFArray, CFBoolean, CFData, CFDictionary, CFNumber, CFRetained, CFString, CFType, CFURL,
    CFURLPathStyle,
};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGBitmapInfo, CGColorSpace, CGContext, CGImageAlphaInfo,
    CGImage, CGImageByteOrderInfo, CGImageComponentInfo, kCGColorSpaceExtendedLinearSRGB,
    kCGColorSpaceSRGB,
};
use objc2_image_io::{
    kCGImageAuxiliaryDataInfoDataDescription, kCGImageAuxiliaryDataInfoMetadata,
    kCGImageAuxiliaryDataTypeHDRGainMap, kCGImageAuxiliaryDataTypeISOGainMap,
    kCGImagePropertyDepth, kCGImagePropertyMakerAppleDictionary, kCGImagePropertyOrientation,
    kCGImagePropertyPixelHeight,
    kCGImagePropertyPixelWidth, kCGImageSourceDecodeRequest, kCGImageSourceDecodeToHDR,
    CGImageMetadata, CGImageMetadataTag, CGImageSource,
};
use tohdr_core::{GainMapMeta, HdrRgb, Rgb};

use crate::{Error, ReadBack, Result};

/// Apple's XMP namespace for the parsed ISO 21496-1 parameters.
const HDR_TONE_MAP_NS: &str = "http://ns.apple.com/HDRToneMap/1.0/";

/// Look a key up in a `CFDictionary` whose values we only know as `CFType`.
///
/// The generated bindings type `CFDictionary` without key/value parameters
/// here, so the lookup goes through raw pointers; the returned reference
/// borrows from `dict`, which is what keeps it alive.
fn get<'a>(dict: &'a CFDictionary, key: &CFString) -> Option<&'a CFType> {
    let ptr = unsafe { dict.value(key as *const CFString as *const c_void) };
    if ptr.is_null() {
        None
    } else {
        Some(unsafe { &*(ptr as *const CFType) })
    }
}

fn get_f64(dict: &CFDictionary, key: &CFString) -> Option<f64> {
    get(dict, key)?.downcast_ref::<CFNumber>()?.as_f64()
}

fn get_u32(dict: &CFDictionary, key: &CFString) -> Option<u32> {
    let v = get(dict, key)?.downcast_ref::<CFNumber>()?.as_i64()?;
    u32::try_from(v).ok()
}

fn get_dict<'a>(dict: &'a CFDictionary, key: &CFString) -> Option<&'a CFDictionary> {
    get(dict, key)?.downcast_ref::<CFDictionary>()
}

/// `CGImageMetadataTag`'s value, as an f64 when it is numeric.
///
/// Apple stores these as `CFString` in some files and `CFNumber` in others, so
/// both are accepted; anything else yields `None`.
fn tag_f64(tag: &CGImageMetadataTag) -> Option<f64> {
    let v = unsafe { tag.value() }?;
    if let Some(n) = v.downcast_ref::<CFNumber>() {
        return n.as_f64();
    }
    v.downcast_ref::<CFString>()?.to_string().trim().parse().ok()
}

fn tag_bool(tag: &CGImageMetadataTag) -> Option<bool> {
    let v = unsafe { tag.value() }?;
    if let Some(b) = v.downcast_ref::<CFBoolean>() {
        return Some(b.as_bool());
    }
    match v.downcast_ref::<CFString>()?.to_string().to_ascii_lowercase().as_str() {
        "true" | "yes" | "1" => Some(true),
        "false" | "no" | "0" => Some(false),
        _ => None,
    }
}

fn tag_name(tag: &CGImageMetadataTag) -> Option<String> {
    unsafe { tag.name() }.map(|s| s.to_string())
}

/// Walk a `CGImageMetadata`'s top-level tags, keeping only the HDRToneMap ones.
fn tone_map_tags(md: &CGImageMetadata) -> Vec<CFRetained<CGImageMetadataTag>> {
    let Some(tags) = (unsafe { md.tags() }) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for i in 0..tags.count() {
        let raw = unsafe { tags.value_at_index(i) };
        if raw.is_null() {
            continue;
        }
        let tag = unsafe { &*(raw as *const CGImageMetadataTag) };
        let ns = unsafe { tag.namespace() }.map(|s| s.to_string());
        if ns.as_deref() == Some(HDR_TONE_MAP_NS) {
            out.push(unsafe { CFRetained::retain(core::ptr::NonNull::from(tag)) });
        }
    }
    out
}

/// The `ChannelMetadata` array holds tags whose values are dictionaries of the
/// five per-channel parameters. Returns them in channel order.
fn channel_dicts(tag: &CGImageMetadataTag) -> Vec<CFRetained<CFDictionary>> {
    let Some(v) = (unsafe { tag.value() }) else {
        return Vec::new();
    };
    let Some(arr) = v.downcast_ref::<CFArray>() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for i in 0..arr.count() {
        let raw = unsafe { arr.value_at_index(i) };
        if raw.is_null() {
            continue;
        }
        let el = unsafe { &*(raw as *const CFType) };
        // Each element is itself a tag ("HDRToneMap:[0] = {...}") whose value
        // is the dictionary, rather than the dictionary directly.
        let d = if let Some(t) = el.downcast_ref::<CGImageMetadataTag>() {
            unsafe { t.value() }.and_then(|v| v.downcast_ref::<CFDictionary>().map(cf_retain_dict))
        } else {
            el.downcast_ref::<CFDictionary>().map(cf_retain_dict)
        };
        if let Some(d) = d {
            out.push(d);
        }
    }
    out
}

fn cf_retain_dict(d: &CFDictionary) -> CFRetained<CFDictionary> {
    unsafe { CFRetained::retain(core::ptr::NonNull::from(d)) }
}

/// A per-channel parameter, read from a `ChannelMetadata` entry. The values in
/// there are themselves tags, not bare numbers.
fn channel_param(d: &CFDictionary, name: &str) -> Option<f64> {
    let key = CFString::from_str(name);
    let v = get(d, &key)?;
    if let Some(t) = v.downcast_ref::<CGImageMetadataTag>() {
        return tag_f64(t);
    }
    if let Some(n) = v.downcast_ref::<CFNumber>() {
        return n.as_f64();
    }
    v.downcast_ref::<CFString>()?.to_string().trim().parse().ok()
}

/// Rebuild [`GainMapMeta`] from ImageIO's parsed HDRToneMap tags.
///
/// Returns `None` when the headroom tags are absent — a file can carry an aux
/// image with no ISO parameters at all (Apple-only flavor), and that is not an
/// error.
fn meta_from_metadata(md: &CGImageMetadata) -> Option<GainMapMeta> {
    let tags = tone_map_tags(md);
    let mut base_headroom = None;
    let mut alt_headroom = None;
    let mut use_base_color_space = None;
    let mut channels: Vec<CFRetained<CFDictionary>> = Vec::new();

    for t in &tags {
        match tag_name(t).as_deref() {
            Some("BaseHeadroom") => base_headroom = tag_f64(t),
            Some("AlternateHeadroom") => alt_headroom = tag_f64(t),
            Some("BaseColorIsWorkingColor") => use_base_color_space = tag_bool(t),
            Some("ChannelMetadata") => channels = channel_dicts(t),
            _ => {}
        }
    }

    let base_headroom = base_headroom? as f32;
    let alt_headroom = alt_headroom? as f32;

    // Apple writes one channel entry for a monochrome map; ISO permits three.
    // Replicate a single entry across all three so downstream code can index
    // uniformly, matching `GainMapMeta`'s documented convention.
    let mut meta = GainMapMeta {
        base_headroom,
        alt_headroom,
        use_base_color_space: use_base_color_space.unwrap_or(false),
        ..GainMapMeta::default()
    };
    for c in 0..3 {
        let d = channels.get(c).or_else(|| channels.first());
        let Some(d) = d else { continue };
        if let Some(v) = channel_param(d, "GainMapMin") {
            meta.min_log2[c] = v as f32;
        }
        if let Some(v) = channel_param(d, "GainMapMax") {
            meta.max_log2[c] = v as f32;
        }
        if let Some(v) = channel_param(d, "Gamma") {
            meta.gamma[c] = v as f32;
        }
        if let Some(v) = channel_param(d, "BaseOffset") {
            meta.base_offset[c] = v as f32;
        }
        if let Some(v) = channel_param(d, "AlternateOffset") {
            meta.alt_offset[c] = v as f32;
        }
    }
    Some(meta)
}

/// One flavor's auxiliary-data dictionary, if the file carries it.
fn aux_info(
    isrc: &CGImageSource,
    index: usize,
    kind: &CFString,
) -> Option<CFRetained<CFDictionary>> {
    unsafe { isrc.auxiliary_data_info_at_index(index, kind) }
}

/// MakerApple tags 33 and 48, which Apple keys by their decimal number.
fn maker_apple_tags(props: &CFDictionary) -> (Option<f64>, Option<f64>) {
    let key: &CFString = unsafe { kCGImagePropertyMakerAppleDictionary };
    let Some(maker) = get_dict(props, key) else {
        return (None, None);
    };
    let t33 = get_f64(maker, &CFString::from_str("33"));
    let t48 = get_f64(maker, &CFString::from_str("48"));
    (t33, t48)
}

fn analyze(isrc: &CGImageSource) -> Result<ReadBack> {
    let index = unsafe { isrc.primary_image_index() };
    let props = unsafe { isrc.properties_at_index(index, None) }
        .ok_or(Error::Missing("image properties"))?;

    let width = get_u32(&props, unsafe { kCGImagePropertyPixelWidth }).unwrap_or(0);
    let height = get_u32(&props, unsafe { kCGImagePropertyPixelHeight }).unwrap_or(0);
    let depth = get_u32(&props, unsafe { kCGImagePropertyDepth }).unwrap_or(0);
    // ImageIO resolves `irot`/`imir` back into one Exif orientation number, so
    // this is the oracle for whether a container transform we wrote means what we
    // meant — the one question `tohdr_core::orient`'s own tests cannot answer,
    // since they assume the order the boxes compose in.
    let orientation = get_u32(&props, unsafe { kCGImagePropertyOrientation });
    let (tag33, tag48) = maker_apple_tags(&props);

    let apple = aux_info(isrc, index, unsafe { kCGImageAuxiliaryDataTypeHDRGainMap });
    let iso = aux_info(isrc, index, unsafe { kCGImageAuxiliaryDataTypeISOGainMap });

    // Either flavor's dictionary describes the same plane in `IMG_4913.HEIC`;
    // prefer ISO's when both are present, since that is the one carrying the
    // parameters we also want.
    let plane_src = iso.as_ref().or(apple.as_ref());
    let mut gain_size = None;
    let mut gain_pixel_format = None;
    if let Some(d) = plane_src {
        let key: &CFString = unsafe { kCGImageAuxiliaryDataInfoDataDescription };
        if let Some(desc) = get_dict(d, key) {
            let w = get_u32(desc, &CFString::from_str("Width"));
            let h = get_u32(desc, &CFString::from_str("Height"));
            if let (Some(w), Some(h)) = (w, h) {
                gain_size = Some((w, h));
            }
            gain_pixel_format = get_u32(desc, &CFString::from_str("PixelFormat"));
        }
    }

    let iso_meta = iso.as_ref().and_then(|d| {
        let key: &CFString = unsafe { kCGImageAuxiliaryDataInfoMetadata };
        let md = get(d, key)?.downcast_ref::<CGImageMetadata>()?;
        meta_from_metadata(md)
    });

    let apple_headroom = tag33
        .zip(tag48)
        .map(|(a, b)| tohdr_core::apple::headroom_from_tags(a, b));

    Ok(ReadBack {
        width,
        height,
        depth,
        apple_aux: apple.is_some(),
        iso_aux: iso.is_some(),
        gain_size,
        gain_pixel_format,
        tag33,
        tag48,
        apple_headroom,
        iso_meta,
        orientation,
    })
}

/// Decode any ImageIO-supported file, optionally applying its gain map, into a
/// bitmap we own.
///
/// Everything goes through a `CGContext` render rather than poking at the
/// `CGImage`'s own buffer: ImageIO hands back whatever layout the file
/// happened to use (subsampled chroma, planar, 10-bit packed), and drawing
/// into a context we allocated is the supported way to get one known layout
/// out of all of them.
struct Decoded {
    image: CFRetained<CGImage>,
    w: usize,
    h: usize,
}

/// Open a file through ImageIO and take its primary image.
///
/// No pixels are produced here. `CGImage` is lazy: the decode happens on the
/// first draw, which is why band count does not multiply it.
fn open_image(path: &Path, hdr: bool) -> Result<Decoded> {
    let s = path.to_str().ok_or_else(|| {
        Error::Unreadable(format!("path is not valid UTF-8: {}", path.display()))
    })?;
    let cfpath = CFString::from_str(s);
    let url = CFURL::with_file_system_path(
        None,
        Some(&cfpath),
        CFURLPathStyle::CFURLPOSIXPathStyle,
        false,
    )
    .ok_or(Error::NullFromFramework("CFURLCreateWithFileSystemPath"))?;

    // `kCGImageSourceDecodeRequest` = `...RequestHDRImage` is what makes
    // ImageIO apply a gain map during decode instead of handing back the SDR
    // base; without it an HDR HEIC silently decodes as its base image.
    let opts = if hdr {
        let key: &CFString = unsafe { kCGImageSourceDecodeRequest };
        let val: &CFString = unsafe { kCGImageSourceDecodeToHDR };
        let keys = [key];
        let vals = [val];
        Some(CFDictionary::<CFString, CFString>::from_slices(&keys, &vals))
    } else {
        None
    };
    let opts_ref = opts.as_ref().map(|d| {
        let o: &CFDictionary = d.as_opaque();
        o
    });

    let isrc = unsafe { CGImageSource::with_url(&url, opts_ref) }
        .ok_or_else(|| Error::Unreadable(format!("ImageIO could not open {}", path.display())))?;
    let index = unsafe { isrc.primary_image_index() };
    let image = unsafe { isrc.image_at_index(index, opts_ref) }
        .ok_or(Error::Missing("primary image"))?;

    let w = CGImage::width(Some(&image));
    let h = CGImage::height(Some(&image));
    if w == 0 || h == 0 {
        return Err(Error::Missing("nonzero image dimensions"));
    }
    Ok(Decoded { image, w, h })
}

/// Total scratch for all in-flight bands. Bands are drawn concurrently, so this
/// is a budget for the whole render rather than per thread.
const BAND_BUDGET: usize = 256 << 20;

/// A `CGImage` several threads draw from at once, with the colour space they
/// share.
///
/// Both are immutable once created and CoreGraphics supports drawing one image
/// into independent contexts concurrently; the generated bindings simply do not
/// carry that in the type. `examples/probe_band_parallel.rs` measures it: 4.8x
/// on the draw stage at ten threads, and the banded output is byte-identical to
/// the single-threaded full-frame render.
struct SharedImage<'a> {
    image: &'a CGImage,
    cs: &'a CGColorSpace,
}

// SAFETY: read-only use of two immutable Core Foundation objects. Nothing here
// mutates either one, and each thread creates its own `CGContext`.
unsafe impl Send for SharedImage<'_> {}
unsafe impl Sync for SharedImage<'_> {}

/// The parts of the render target's layout that every band shares.
#[derive(Clone, Copy)]
struct BandFmt {
    w: usize,
    h: usize,
    bits: usize,
    stride: usize,
    bitmap: CGBitmapInfo,
}

/// Draw rows `[y0, y0 + rows)` of the image into `buf`.
fn draw_band(sh: &SharedImage, f: BandFmt, buf: &mut [u8], y0: usize, rows: usize) -> Result<()> {
    // `draw_image` composites rather than replaces, so a source pixel with
    // alpha below one would otherwise show the previous band through it.
    buf[..f.stride * rows].fill(0);
    let ctx = unsafe {
        CGBitmapContextCreate(
            buf.as_mut_ptr() as *mut c_void,
            f.w,
            rows,
            f.bits,
            f.stride,
            Some(sh.cs),
            f.bitmap.0,
        )
    }
    .ok_or(Error::NullFromFramework("CGBitmapContextCreate"))?;
    // CoreGraphics' origin is bottom-left while the band's memory starts at its
    // top row, so placing the whole image at this offset leaves exactly rows
    // [y0, y0 + rows) inside the context.
    CGContext::draw_image(
        Some(&ctx),
        CGRect {
            origin: CGPoint { x: 0.0, y: (rows + y0) as f64 - f.h as f64 },
            size: CGSize { width: f.w as f64, height: f.h as f64 },
        },
        Some(sh.image),
    );
    Ok(())
}

/// Render into `out` in horizontal bands, converting each with `convert`.
///
/// Two things fall out of banding. The full-frame target used to be the largest
/// allocation in the program — 918 MiB of RGBA f32 at 60 Mpx, alive only long
/// enough to be de-interleaved and dropped — and bands cap it at `BAND_BUDGET`.
/// More importantly the bands are independent: the render splits into a ~1150 ms
/// one-off decode charged to the first draw and ~2020 ms of area-proportional
/// conversion, and only the second part is on the critical path once threads
/// divide it. Measured on a 60 Mpx raw, total render 3165 ms serial against
/// 1585 ms across ten threads.
///
/// `convert` receives one band's destination rows and the raw bytes behind
/// them, and runs on the thread that drew the band, while other threads are
/// still drawing theirs.
fn render_banded_into<T, F>(
    d: &Decoded,
    hdr: bool,
    out: &mut [T],
    row_len: usize,
    convert: F,
) -> Result<()>
where
    T: Send,
    F: Fn(&mut [T], &[u8], usize) + Sync,
{
    let (w, h) = (d.w, d.h);

    // 32-bit float RGBA in extended linear sRGB for HDR (so above-white
    // survives), 8-bit RGBA sRGB for SDR.
    let (cs_name, bits, bytes_per_px) = if hdr {
        (unsafe { kCGColorSpaceExtendedLinearSRGB }, 32usize, 16usize)
    } else {
        (unsafe { kCGColorSpaceSRGB }, 8usize, 4usize)
    };
    let cs = CGColorSpace::with_name(Some(cs_name))
        .ok_or(Error::NullFromFramework("CGColorSpaceCreateWithName"))?;
    let bitmap = if hdr {
        CGBitmapInfo(
            CGImageComponentInfo::Float.0
                | CGImageByteOrderInfo::Order32Little.0
                | CGImageAlphaInfo::PremultipliedLast.0,
        )
    } else {
        CGBitmapInfo(CGImageAlphaInfo::PremultipliedLast.0)
    };
    let stride = w * bytes_per_px;
    let fmt = BandFmt { w, h, bits, stride, bitmap };
    let sh = SharedImage { image: &d.image, cs: &cs };

    let threads = tohdr_core::par::threads().min(h);
    let band_rows = (BAND_BUDGET / threads / stride.max(1)).clamp(1, h);

    // The one-off decode belongs to whichever draw runs first; forcing it with
    // a single row keeps every worker from arriving at it together.
    draw_band(&sh, fmt, &mut vec![0u8; stride], 0, 1)?;

    let mut bands: Vec<(usize, &mut [T])> = out
        .chunks_mut(band_rows * row_len)
        .enumerate()
        .map(|(i, c)| (i * band_rows, c))
        .collect();

    // Bands are equal-cost, so a static split needs no work stealing. Each
    // worker keeps one scratch buffer for its whole run of bands.
    let per_worker = bands.len().div_ceil(threads);
    let (sh, convert) = (&sh, &convert);
    let mut failed = Ok(());
    std::thread::scope(|s| {
        let mut handles = Vec::with_capacity(threads);
        for group in bands.chunks_mut(per_worker) {
            handles.push(s.spawn(move || -> Result<()> {
                let mut buf = vec![0u8; stride * band_rows];
                for (y0, dst) in group {
                    let rows = dst.len() / row_len;
                    draw_band(sh, fmt, &mut buf, *y0, rows)?;
                    convert(dst, &buf[..stride * rows], bytes_per_px);
                }
                Ok(())
            }));
        }
        for handle in handles {
            match handle.join() {
                Ok(r) => {
                    if r.is_err() && failed.is_ok() {
                        failed = r;
                    }
                }
                Err(_) => failed = Err(Error::Missing("band worker panicked")),
            }
        }
    });
    failed
}

pub(crate) fn load_hdr(path: &Path) -> Result<HdrRgb> {
    let d = open_image(path, true)?;
    let (w, h) = (d.w, d.h);
    let row_len = w * 3;
    let mut data = vec![0f32; row_len * h];
    render_banded_into(&d, true, &mut data, row_len, |dst, band, bpp| {
        let mut o = 0;
        for px in dst.chunks_exact_mut(3) {
            for (c, v) in px.iter_mut().enumerate() {
                let b = &band[o + c * 4..o + c * 4 + 4];
                *v = f32::from_ne_bytes([b[0], b[1], b[2], b[3]]).max(0.0);
            }
            o += bpp;
        }
    })?;
    Ok(HdrRgb { width: w as u32, height: h as u32, data })
}

pub(crate) fn load_sdr(path: &Path) -> Result<Rgb> {
    let d = open_image(path, false)?;
    let (w, h) = (d.w, d.h);
    let row_len = w * 3;
    let mut data = vec![0u16; row_len * h];
    render_banded_into(&d, false, &mut data, row_len, |dst, band, bpp| {
        let mut o = 0;
        for px in dst.chunks_exact_mut(3) {
            px[0] = band[o] as u16;
            px[1] = band[o + 1] as u16;
            px[2] = band[o + 2] as u16;
            o += bpp;
        }
    })?;
    Ok(Rgb { width: w as u32, height: h as u32, bits: 8, data })
}

pub(crate) fn inspect_path(path: &Path) -> Result<ReadBack> {
    let s = path.to_str().ok_or_else(|| {
        Error::Unreadable(format!("path is not valid UTF-8: {}", path.display()))
    })?;
    let cfpath = CFString::from_str(s);
    let url = CFURL::with_file_system_path(None, Some(&cfpath), CFURLPathStyle::CFURLPOSIXPathStyle, false)
        .ok_or(Error::NullFromFramework("CFURLCreateWithFileSystemPath"))?;
    let isrc = unsafe { CGImageSource::with_url(&url, None) }
        .ok_or_else(|| Error::Unreadable(format!("ImageIO could not open {}", path.display())))?;
    analyze(&isrc)
}

pub(crate) fn inspect_bytes(bytes: &[u8]) -> Result<ReadBack> {
    let data = CFData::from_bytes(bytes);
    let isrc = unsafe { CGImageSource::with_data(&data, None) }
        .ok_or_else(|| Error::Unreadable("ImageIO could not open the in-memory buffer".into()))?;
    analyze(&isrc)
}
