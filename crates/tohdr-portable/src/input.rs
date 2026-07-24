//! Pure-Rust decoders for HDR and SDR sources (no Apple frameworks, `image`
//! crate only).
//!
//! # Colour-space assumptions
//!
//! An unmanaged image file carries no single right answer for "what is
//! linear light here" — that's a property of a colour profile this crate does
//! not read. We pick one assumption per format and name it here rather than
//! pretend otherwise:
//!
//! - **Floating-point TIFF** — assumed **scene-linear** with `1.0` at SDR
//!   diffuse white and samples above it carrying real headroom. A float file
//!   can represent above-white light directly, so applying a transfer function
//!   to it would be inventing one; taking the samples as-is is the only
//!   reading that preserves what the file already says.
//! - **Integer TIFF** — [`load_hdr`]'s primary input, matching what Lightroom
//!   Classic's HDR export produces — is assumed **PQ (ST 2084) encoded**: the
//!   16-bit code value is treated as the PQ signal linearly rescaled to
//!   `0..=65535` (`code / 65535`), decoded to nits via the ST 2084 EOTF, then
//!   divided by a reference white so `1.0` lands at SDR diffuse white. That
//!   reference white — [`DEFAULT_REFERENCE_WHITE_NITS`], 203 nits per ITU-R
//!   BT.2408 — is this assumption's one free parameter; call
//!   [`load_hdr_tiff_pq`] directly with a different value if your pipeline
//!   uses another convention (e.g. Apple's 100 nits).
//!
//! Splitting TIFF on sample format is not a nicety. Routing a float TIFF
//! through the integer path costs the image twice over: `to_rgb16` clamps
//! every above-white sample to full code, erasing the headroom, and the PQ
//! EOTF then reads that full code back as 10000 nits. A source with a true
//! 8x peak decodes to a uniform 49.26x plateau — the highlights are gone and
//! the declared headroom is 2.6 stops too high.
//! - **PNG / JPEG** passed to [`load_hdr`]: assumed plain sRGB-encoded SDR
//!   with *no* headroom (`1.0` is also the maximum representable value).
//!   Neither format has a widely-used convention for encoding above-white
//!   samples, so "no headroom" is the honest floor rather than a guess.
//! - [`load_sdr`] always assumes sRGB, for all three formats — SDR files are
//!   display-referred by definition, so there is no headroom question there.
//!
//! Get the assumption wrong for a given file and the image decodes without
//! error but is simply wrong — too flat (an sRGB source read as PQ headroom)
//! or blown out (the reverse). No amount of pixel-data inspection can catch
//! that; only knowing how the file was produced can.

use std::path::Path;

use image::DynamicImage;
use tohdr_core::{HdrRgb, Rgb};

use crate::{Error, Result};

/// ITU-R BT.2408 "reference white" for PQ HDR grading, in nits. The default
/// reference white [`load_hdr`] uses for TIFF; see the module docs for how to
/// override it.
pub const DEFAULT_REFERENCE_WHITE_NITS: f32 = 203.0;

// SMPTE ST 2084 (PQ) EOTF constants, full precision per the spec (computed in
// f64, cast down at the end — the intermediate `np.powf` terms are sensitive
// enough near 0 that f32 visibly perturbs the toe).
const PQ_M1: f64 = 2610.0 / 16384.0;
const PQ_M2: f64 = 2523.0 / 4096.0 * 128.0;
const PQ_C1: f64 = 3424.0 / 4096.0;
const PQ_C2: f64 = 2413.0 / 4096.0 * 32.0;
const PQ_C3: f64 = 2392.0 / 4096.0 * 32.0;

/// ST 2084 EOTF: normalized PQ code `n` in `0.0..=1.0` to nits in
/// `0.0..=10000.0`.
pub(crate) fn pq_to_nits(n: f32) -> f32 {
    let n = (n as f64).clamp(0.0, 1.0);
    let np = n.powf(1.0 / PQ_M2);
    let num = (np - PQ_C1).max(0.0);
    let den = PQ_C2 - PQ_C3 * np;
    let l = if den <= 0.0 {
        0.0
    } else {
        (num / den).powf(1.0 / PQ_M1)
    };
    (l * 10000.0) as f32
}

/// True sRGB EOTF: piecewise linear toe below `0.04045`, power curve above.
/// Matches `tohdr_core::derive`'s private copy of the same formula (kept
/// separate here since that one is `pub(crate)` to its own crate).
pub(crate) fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn decode_any(path: &Path) -> Result<DynamicImage> {
    image::ImageReader::open(path)?
        .with_guessed_format()?
        .decode()
        .map_err(|e| Error::Decode(e.to_string()))
}

fn extension_lower(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
}

/// Decode a floating-point TIFF as scene-linear, `1.0` at SDR diffuse white
/// (see module docs). Samples pass through untouched, headroom included.
///
/// Negative samples are clamped to zero: they are legal in a float file (some
/// wide-gamut encodings put out-of-gamut colours below zero) but a gain map is
/// a ratio of luminances, and a negative luminance has no meaning downstream.
fn load_hdr_tiff_linear(img: DynamicImage) -> HdrRgb {
    let rgb32f = img.to_rgb32f();
    let (w, h) = (rgb32f.width(), rgb32f.height());
    let data = rgb32f.into_raw().into_iter().map(|v| v.max(0.0)).collect();
    HdrRgb { width: w, height: h, data }
}

/// Decode a 16-bit TIFF assumed PQ-encoded (see module docs), with an
/// explicit reference white — the override entry point for [`load_hdr`]'s
/// default of [`DEFAULT_REFERENCE_WHITE_NITS`].
///
/// Forces the PQ reading even for a float TIFF, which [`load_hdr`] would treat
/// as linear; that is the point of it being a separate public entry point.
pub fn load_hdr_tiff_pq(path: &Path, reference_white_nits: f32) -> Result<HdrRgb> {
    let img = decode_any(path)?;
    check_size(&img)?;
    Ok(tiff_pq_from(img, reference_white_nits))
}

/// Largest image accepted, in pixels. `image`'s own decode limit bounds the
/// buffer for the file's *original* format; our `to_rgb16`/`to_rgb32f`
/// widening happens after that and is uncapped, so a 1-byte-per-pixel
/// greyscale source that decodes inside the limit can still demand 6x that
/// on conversion. A failed `Vec` growth aborts rather than panicking, which
/// `panic_guard` cannot catch — so bound it here instead.
const MAX_PIXELS: u64 = 512 * 1024 * 1024;

fn check_size(img: &DynamicImage) -> Result<()> {
    let (w, h) = (img.width() as u64, img.height() as u64);
    let n = w.saturating_mul(h);
    if n > MAX_PIXELS {
        return Err(Error::UnsupportedInput(format!(
            "{w}x{h} is {n} pixels, over the {MAX_PIXELS}-pixel limit"
        )));
    }
    Ok(())
}

fn srgb_no_headroom(img: DynamicImage) -> HdrRgb {
    let rgb8 = img.to_rgb8();
    let (w, h) = (rgb8.width(), rgb8.height());
    let data = rgb8
        .pixels()
        .flat_map(|p| p.0)
        .map(|s| srgb_to_linear(s as f32 / 255.0))
        .collect();
    HdrRgb { width: w, height: h, data }
}

fn tiff_pq_from(img: DynamicImage, reference_white_nits: f32) -> HdrRgb {
    let rgb16 = img.to_rgb16();
    let (w, h) = (rgb16.width(), rgb16.height());
    let mut data = Vec::with_capacity(w as usize * h as usize * 3);
    for px in rgb16.pixels() {
        for &s in &px.0 {
            let n = s as f32 / u16::MAX as f32;
            data.push(pq_to_nits(n) / reference_white_nits);
        }
    }
    HdrRgb { width: w, height: h, data }
}

/// Decode a PNG/JPEG assumed sRGB-encoded with no headroom (see module docs).
fn load_hdr_srgb_no_headroom(path: &Path) -> Result<HdrRgb> {
    let img = decode_any(path)?;
    let rgb8 = img.to_rgb8();
    let (w, h) = (rgb8.width(), rgb8.height());
    let mut data = Vec::with_capacity(w as usize * h as usize * 3);
    for px in rgb8.pixels() {
        for &s in &px.0 {
            data.push(srgb_to_linear(s as f32 / 255.0));
        }
    }
    Ok(HdrRgb { width: w, height: h, data })
}

/// Decode an HDR source with pure-Rust decoders only. Dispatches on file
/// extension; see the module docs for the colour-space assumption each format
/// gets and how to override it.
pub fn load_hdr(path: &Path) -> Result<HdrRgb> {
    match extension_lower(path).as_deref() {
        Some("tif") | Some("tiff") => {
            let img = decode_any(path)?;
            check_size(&img)?;
            // Sample format, not extension, decides the transfer function; see
            // the module docs for what routing a float TIFF through PQ costs.
            match img {
                DynamicImage::ImageRgb32F(_) | DynamicImage::ImageRgba32F(_) => {
                    Ok(load_hdr_tiff_linear(img))
                }
                DynamicImage::ImageRgb16(_) | DynamicImage::ImageRgba16(_)
                | DynamicImage::ImageLuma16(_) | DynamicImage::ImageLumaA16(_) => {
                    Ok(tiff_pq_from(img, DEFAULT_REFERENCE_WHITE_NITS))
                }
                // An 8-bit TIFF has no headroom to recover, and reading its
                // codes as PQ is actively wrong: mid-grey 128 would decode to
                // ~0.47x SDR white instead of ~0.22x. Treat it as what it is —
                // an SDR sRGB image — the same as PNG and JPEG.
                _ => Ok(srgb_no_headroom(img)),
            }
        }
        Some("png") | Some("jpg") | Some("jpeg") => load_hdr_srgb_no_headroom(path),
        other => Err(Error::UnsupportedInput(format!(
            "load_hdr: unsupported extension {other:?} (want tif/tiff/png/jpg/jpeg)"
        ))),
    }
}

/// Decode an SDR source with pure-Rust decoders only, assumed sRGB-encoded.
pub fn load_sdr(path: &Path) -> Result<Rgb> {
    match extension_lower(path).as_deref() {
        Some("tif") | Some("tiff") | Some("png") | Some("jpg") | Some("jpeg") => {
            let img = decode_any(path)?;
            let rgb8 = img.to_rgb8();
            let (w, h) = (rgb8.width(), rgb8.height());
            let data = rgb8.into_raw().into_iter().map(|b| b as u16).collect();
            Ok(Rgb { width: w, height: h, bits: 8, data })
        }
        other => Err(Error::UnsupportedInput(format!(
            "load_sdr: unsupported extension {other:?} (want tif/tiff/png/jpg/jpeg)"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, Rgb as ImgRgb};

    #[test]
    fn pq_endpoints_and_monotonic() {
        assert!(pq_to_nits(0.0).abs() < 1e-6);
        assert!((pq_to_nits(1.0) - 10000.0).abs() < 1.0);
        let mut prev = -1.0f32;
        for i in 0..=20 {
            let n = pq_to_nits(i as f32 / 20.0);
            assert!(n >= prev, "pq_to_nits must be monotonic");
            prev = n;
        }
    }

    #[test]
    fn srgb_endpoints_and_known_midpoint() {
        assert!(srgb_to_linear(0.0).abs() < 1e-6);
        assert!((srgb_to_linear(1.0) - 1.0).abs() < 1e-6);
        // Textbook sRGB EOTF value at code 0.5, widely cited (e.g. Poynton).
        assert!((srgb_to_linear(0.5) - 0.214041).abs() < 1e-4);
    }

    fn write_tiff16(path: &Path, w: u32, h: u32, value: u16) {
        let img: ImageBuffer<ImgRgb<u16>, Vec<u16>> =
            ImageBuffer::from_pixel(w, h, ImgRgb([value, value, value]));
        img.save(path).expect("write tiff");
    }

    fn write_png8(path: &Path, w: u32, h: u32, value: u8) {
        let img: ImageBuffer<ImgRgb<u8>, Vec<u8>> =
            ImageBuffer::from_pixel(w, h, ImgRgb([value, value, value]));
        img.save(path).expect("write png");
    }

    #[test]
    fn tiff_full_white_code_produces_headroom_above_one() {
        let dir = std::env::temp_dir();
        let path = dir.join("tohdr_portable_test_pq_white.tiff");
        write_tiff16(&path, 4, 4, u16::MAX);
        let hdr = load_hdr(&path).expect("decode");
        assert_eq!((hdr.width, hdr.height), (4, 4));
        // Full-code PQ is 10000 nits; at the default 203-nit reference white
        // that's real headroom, not merely SDR white.
        assert!(hdr.data.iter().all(|&v| v > 1.0));
        let _ = std::fs::remove_file(&path);
    }

    fn write_tiff32f(path: &Path, w: u32, h: u32, px: &[[f32; 3]]) {
        let mut buf: ImageBuffer<ImgRgb<f32>, Vec<f32>> = ImageBuffer::new(w, h);
        for (i, p) in buf.pixels_mut().enumerate() {
            *p = ImgRgb(px[i % px.len()]);
        }
        buf.save(path).expect("write f32 tiff");
    }

    /// The regression this dispatch exists to prevent. A float TIFF used to be
    /// routed through the 16-bit PQ path, which clamped every above-white
    /// sample to full code and then decoded it as 10000 nits: an 8x peak came
    /// back as 49.26x (10000/203), highlights flattened into one plateau.
    #[test]
    fn float_tiff_is_linear_not_pq() {
        let dir = std::env::temp_dir();
        let path = dir.join("tohdr_portable_test_linear_f32.tiff");
        // Two distinct above-white levels, so a clamp shows up as them merging.
        write_tiff32f(&path, 4, 2, &[[8.0, 8.0, 8.0], [2.0, 2.0, 2.0]]);
        let hdr = load_hdr(&path).expect("decode");

        let max = hdr.data.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
        assert!((max - 8.0).abs() < 1e-3, "expected the 8x peak, got {max}");
        assert!(
            (max - 10000.0 / DEFAULT_REFERENCE_WHITE_NITS).abs() > 1.0,
            "decoded to the PQ plateau: the float TIFF took the integer path"
        );
        assert!(
            hdr.data.iter().any(|&v| (v - 2.0).abs() < 1e-3),
            "the lower highlight level was crushed away"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// The PQ entry point stays reachable for a float file when the caller
    /// explicitly asks for that reading.
    #[test]
    fn explicit_pq_entry_point_still_forces_pq_on_a_float_tiff() {
        let dir = std::env::temp_dir();
        let path = dir.join("tohdr_portable_test_forced_pq.tiff");
        write_tiff32f(&path, 2, 2, &[[1.0, 1.0, 1.0]]);
        let hdr = load_hdr_tiff_pq(&path, DEFAULT_REFERENCE_WHITE_NITS).expect("decode");
        let max = hdr.data.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
        assert!((max - 10000.0 / DEFAULT_REFERENCE_WHITE_NITS).abs() < 1.0);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn negative_float_samples_clamp_to_zero() {
        let dir = std::env::temp_dir();
        let path = dir.join("tohdr_portable_test_negative_f32.tiff");
        write_tiff32f(&path, 2, 2, &[[-0.5, 0.25, 3.0]]);
        let hdr = load_hdr(&path).expect("decode");
        assert!(hdr.data.iter().all(|&v| v >= 0.0));
        assert!(hdr.data.iter().any(|&v| (v - 3.0).abs() < 1e-3));
        let _ = std::fs::remove_file(&path);
    }

    fn write_tiff8(path: &Path, w: u32, h: u32, value: u8) {
        let img: ImageBuffer<ImgRgb<u8>, Vec<u8>> =
            ImageBuffer::from_pixel(w, h, ImgRgb([value, value, value]));
        img.save(path).expect("write 8-bit tiff");
    }

    /// An 8-bit TIFF has no headroom to recover and is not PQ-coded. Reading
    /// its codes through the PQ EOTF put mid-grey at ~0.47x SDR white instead
    /// of ~0.22x — a silently, badly wrong image.
    #[test]
    fn eight_bit_tiff_is_srgb_not_pq() {
        let dir = std::env::temp_dir();
        let path = dir.join("tohdr_portable_test_8bit.tiff");
        write_tiff8(&path, 4, 4, 128);
        let hdr = load_hdr(&path).expect("decode");
        let v = hdr.data[0];
        let want = srgb_to_linear(128.0 / 255.0);
        assert!(
            (v - want).abs() < 1e-4,
            "8-bit TIFF mid-grey should be sRGB {want:.4}, got {v:.4}"
        );
        assert!(
            hdr.data.iter().all(|&x| x <= 1.0 + 1e-6),
            "an 8-bit source cannot carry headroom"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// 16-bit keeps the documented PQ reading.
    #[test]
    fn sixteen_bit_tiff_still_takes_the_pq_path() {
        let dir = std::env::temp_dir();
        let path = dir.join("tohdr_portable_test_16bit_pq.tiff");
        write_tiff16(&path, 4, 4, u16::MAX);
        let hdr = load_hdr(&path).expect("decode");
        let max = hdr.data.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
        assert!((max - 10000.0 / DEFAULT_REFERENCE_WHITE_NITS).abs() < 1.0);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn tiff_zero_code_is_black() {
        let dir = std::env::temp_dir();
        let path = dir.join("tohdr_portable_test_pq_black.tiff");
        write_tiff16(&path, 4, 4, 0);
        let hdr = load_hdr(&path).expect("decode");
        assert!(hdr.data.iter().all(|&v| v.abs() < 1e-6));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn png_load_hdr_has_no_headroom() {
        let dir = std::env::temp_dir();
        let path = dir.join("tohdr_portable_test_srgb_white.png");
        write_png8(&path, 4, 4, 255);
        let hdr = load_hdr(&path).expect("decode");
        // sRGB no-headroom assumption: full-code white is exactly 1.0, never
        // above it.
        assert!(hdr.data.iter().all(|&v| (v - 1.0).abs() < 1e-4));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_sdr_png_roundtrip_shape_and_range() {
        let dir = std::env::temp_dir();
        let path = dir.join("tohdr_portable_test_sdr.png");
        write_png8(&path, 8, 6, 128);
        let rgb = load_sdr(&path).expect("decode");
        assert_eq!((rgb.width, rgb.height, rgb.bits), (8, 6, 8));
        assert_eq!(rgb.data.len(), rgb.expected_len());
        assert!(rgb.data.iter().all(|&v| v <= rgb.max_value()));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn unsupported_extension_errors() {
        let path = Path::new("/tmp/does-not-matter.bmp");
        assert!(matches!(load_hdr(path), Err(Error::UnsupportedInput(_))));
        assert!(matches!(load_sdr(path), Err(Error::UnsupportedInput(_))));
    }
}
