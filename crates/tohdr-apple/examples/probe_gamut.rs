//! How much colour does rendering into sRGB primaries throw away?
//!
//! `tohdr_apple::load_hdr` renders every source into
//! `kCGColorSpaceExtendedLinearSRGB` and then clamps each component with
//! `.max(0.0)` (`read.rs`). That clamp is the point of no return for
//! wide-gamut colour: extended-range spaces *can* represent a colour outside
//! their primaries — as one or more negative components — so the colour
//! survives the colour-space conversion and dies at the clamp.
//!
//! This measures what dies. For each file:
//!
//!   1. render into extended-linear sRGB (what the pipeline actually does),
//!   2. render into extended-linear Display P3,
//!   3. cross-check the two by matrix, because if CoreGraphics gamut-*maps*
//!      instead of preserving negatives then every number below is vacuous —
//!      the 709 render would already read as in-gamut and there would be
//!      nothing to count. This is why there are two renders rather than one.
//!
//! Then it reports what fraction of pixels fall outside Rec.709, Display P3
//! and Rec.2020, and the CIEDE2000 difference between each pixel's true
//! colour and the colour left after the clamp.
//!
//! Read-only: opens sources through ImageIO and writes nothing.
//!
//! # Measured
//!
//! ```text
//!                             outside    dE>=1     dE>=3    worst
//!                             Rec.709   of image  of image     dE
//!   IMG_4913.HEIC (P3)          0.18%     0.01%     0.00%    2.63
//!   DSC07746.ARW (60 MP)       39.41%     1.79%     0.18%    5.48
//! ```
//!
//! Both cross-checks agreed to <2e-4, so CoreGraphics does preserve
//! out-of-gamut colour through an extended-linear conversion; the clamp in
//! `load_hdr`, not the colour space, is what discards it.
//!
//! The two columns say different things and both matter. 39% of the raw is
//! *technically* outside Rec.709 but only just — the deepest excursion is 8.3%
//! of its own pixel's peak — so the perceptual cost lands on 1.79% of the frame
//! and is obvious on 0.18%. A large out-of-gamut fraction is not by itself
//! evidence of a large loss, which is why this reports CIEDE2000 rather than
//! stopping at the count.
//!
//! `DSC07746.ARW` is developed here by *ImageIO*, not Lightroom, and its
//! sidecar is ignored — so that row is a floor for a near-neutral develop
//! (`crs:Saturation="0"`, all HSL zero, `crs:Vibrance="+15"`), not a
//! measurement of a Lightroom ProPhoto export.

use std::ffi::c_void;
use std::path::Path;

use objc2_core_foundation::{CFDictionary, CFRetained, CFString, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    kCGColorSpaceExtendedLinearDisplayP3, kCGColorSpaceExtendedLinearSRGB, CGBitmapContextCreate,
    CGBitmapInfo, CGColorSpace, CGContext, CGImage, CGImageAlphaInfo, CGImageByteOrderInfo,
    CGImageComponentInfo,
};
use objc2_core_foundation::{CFURL, CFURLPathStyle};
use objc2_image_io::{
    kCGImageSourceDecodeRequest, kCGImageSourceDecodeToHDR, CGImageSource,
};

// --- linear-RGB <-> XYZ, all D65, so no chromatic adaptation is involved ---

const RGB709_TO_XYZ: [[f64; 3]; 3] = [
    [0.4123907993, 0.3575843394, 0.1804807884],
    [0.2126390059, 0.7151686788, 0.0721923154],
    [0.0193308187, 0.1191947798, 0.9505321522],
];
const XYZ_TO_RGB709: [[f64; 3]; 3] = [
    [3.2409699419, -1.5373831776, -0.4986107603],
    [-0.9692436363, 1.8759675015, 0.0415550574],
    [0.0556300797, -0.2039769589, 1.0569715142],
];
const P3_TO_XYZ: [[f64; 3]; 3] = [
    [0.4865709486, 0.2656676932, 0.1982172852],
    [0.2289745641, 0.6917385218, 0.0792869141],
    [0.0000000000, 0.0451133819, 1.0439443689],
];
const XYZ_TO_P3: [[f64; 3]; 3] = [
    [2.4934969119, -0.9313836179, -0.4027107845],
    [-0.8294889696, 1.7626640603, 0.0236246858],
    [0.0358458302, -0.0761723893, 0.9568845240],
];
const XYZ_TO_2020: [[f64; 3]; 3] = [
    [1.7166511880, -0.3556707838, -0.2533662814],
    [-0.6666843518, 1.6164812366, 0.0157685458],
    [0.0176398574, -0.0427706133, 0.9421031212],
];

fn apply(m: [[f64; 3]; 3], v: [f64; 3]) -> [f64; 3] {
    [
        m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
        m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
        m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
    ]
}

/// D65 2-degree white, the reference both primaries sets are defined against.
const WHITE: [f64; 3] = [0.9504559271, 1.0, 1.0890577508];

fn lab(xyz: [f64; 3]) -> [f64; 3] {
    fn f(t: f64) -> f64 {
        const D: f64 = 6.0 / 29.0;
        if t > D * D * D {
            t.cbrt()
        } else {
            t / (3.0 * D * D) + 4.0 / 29.0
        }
    }
    let fx = f(xyz[0] / WHITE[0]);
    let fy = f(xyz[1] / WHITE[1]);
    let fz = f(xyz[2] / WHITE[2]);
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

/// CIEDE2000, the perceptual metric the numbers below are quoted in. ~1.0 is
/// the nominal just-noticeable difference for adjacent patches.
fn delta_e00(p: [f64; 3], q: [f64; 3]) -> f64 {
    let (l1, a1, b1) = (p[0], p[1], p[2]);
    let (l2, a2, b2) = (q[0], q[1], q[2]);
    let c1 = (a1 * a1 + b1 * b1).sqrt();
    let c2 = (a2 * a2 + b2 * b2).sqrt();
    let cbar = (c1 + c2) / 2.0;
    let pow25_7 = 25f64.powi(7);
    let g = 0.5 * (1.0 - (cbar.powi(7) / (cbar.powi(7) + pow25_7)).sqrt());
    let a1p = a1 * (1.0 + g);
    let a2p = a2 * (1.0 + g);
    let c1p = (a1p * a1p + b1 * b1).sqrt();
    let c2p = (a2p * a2p + b2 * b2).sqrt();
    let hp = |b: f64, ap: f64| {
        if b == 0.0 && ap == 0.0 {
            0.0
        } else {
            b.atan2(ap).to_degrees().rem_euclid(360.0)
        }
    };
    let h1p = hp(b1, a1p);
    let h2p = hp(b2, a2p);
    let dlp = l2 - l1;
    let dcp = c2p - c1p;
    let dhp = if c1p * c2p == 0.0 {
        0.0
    } else {
        let d = h2p - h1p;
        if d.abs() <= 180.0 {
            d
        } else if d > 180.0 {
            d - 360.0
        } else {
            d + 360.0
        }
    };
    let dbig_hp = 2.0 * (c1p * c2p).sqrt() * (dhp.to_radians() / 2.0).sin();
    let lbar = (l1 + l2) / 2.0;
    let cbarp = (c1p + c2p) / 2.0;
    let hbarp = if c1p * c2p == 0.0 {
        h1p + h2p
    } else if (h1p - h2p).abs() <= 180.0 {
        (h1p + h2p) / 2.0
    } else if h1p + h2p < 360.0 {
        (h1p + h2p + 360.0) / 2.0
    } else {
        (h1p + h2p - 360.0) / 2.0
    };
    let t = 1.0 - 0.17 * (hbarp - 30.0).to_radians().cos()
        + 0.24 * (2.0 * hbarp).to_radians().cos()
        + 0.32 * (3.0 * hbarp + 6.0).to_radians().cos()
        - 0.20 * (4.0 * hbarp - 63.0).to_radians().cos();
    let dtheta = 30.0 * (-(((hbarp - 275.0) / 25.0).powi(2))).exp();
    let rc = 2.0 * (cbarp.powi(7) / (cbarp.powi(7) + pow25_7)).sqrt();
    let sl = 1.0 + (0.015 * (lbar - 50.0).powi(2)) / (20.0 + (lbar - 50.0).powi(2)).sqrt();
    let sc = 1.0 + 0.045 * cbarp;
    let sh = 1.0 + 0.015 * cbarp * t;
    let rt = -(2.0 * dtheta.to_radians()).sin() * rc;
    ((dlp / sl).powi(2)
        + (dcp / sc).powi(2)
        + (dbig_hp / sh).powi(2)
        + rt * (dcp / sc) * (dbig_hp / sh))
        .sqrt()
}

/// Histogram of CIEDE2000 in 0.05 buckets up to 30, for percentiles without
/// keeping every pixel's value.
struct Hist {
    buckets: Vec<u64>,
    n: u64,
    sum: f64,
    max: f64,
}

impl Hist {
    const STEP: f64 = 0.05;
    fn new() -> Self {
        Hist { buckets: vec![0; 601], n: 0, sum: 0.0, max: 0.0 }
    }
    fn push(&mut self, v: f64) {
        self.n += 1;
        self.sum += v;
        if v > self.max {
            self.max = v;
        }
        let i = ((v / Self::STEP) as usize).min(self.buckets.len() - 1);
        self.buckets[i] += 1;
    }
    fn mean(&self) -> f64 {
        if self.n == 0 {
            0.0
        } else {
            self.sum / self.n as f64
        }
    }
    /// Lower edge of the bucket the given quantile falls in.
    fn quantile(&self, q: f64) -> f64 {
        if self.n == 0 {
            return 0.0;
        }
        let target = (q * self.n as f64) as u64;
        let mut seen = 0u64;
        for (i, c) in self.buckets.iter().enumerate() {
            seen += c;
            if seen >= target {
                return i as f64 * Self::STEP;
            }
        }
        self.max
    }
    /// Fraction of samples at or above `v`.
    fn fraction_above(&self, v: f64) -> f64 {
        if self.n == 0 {
            return 0.0;
        }
        let from = (v / Self::STEP) as usize;
        let c: u64 = self.buckets[from.min(self.buckets.len() - 1)..].iter().sum();
        c as f64 / self.n as f64
    }
}

#[derive(Default)]
struct Counts {
    total: u64,
    /// Pixels the extended-linear-sRGB render itself reports as out of gamut,
    /// i.e. with a negative component. Zero here while `out_709_by_matrix` is
    /// large would mean CoreGraphics clipped and the measurement is void.
    negative_in_709_render: u64,
    out_709_by_matrix: u64,
    out_p3: u64,
    out_2020: u64,
    above_white: u64,
    /// Largest single negative component seen, and how deep it is relative to
    /// that pixel's brightest component.
    worst_negative: f64,
    worst_negative_relative: f64,
    /// Largest disagreement between the two renders after converting P3 -> 709
    /// by matrix. Small = the two agree = neither render was clipped.
    max_render_disagreement: f64,
}

fn open_image(path: &Path) -> Option<(CFRetained<CGImage>, usize, usize)> {
    let cfpath = CFString::from_str(path.to_str()?);
    let url = CFURL::with_file_system_path(
        None,
        Some(&cfpath),
        CFURLPathStyle::CFURLPOSIXPathStyle,
        false,
    )?;
    // Same request `load_hdr` makes, so this measures the pixels the pipeline
    // actually sees rather than an SDR rendition of them.
    let key: &CFString = unsafe { kCGImageSourceDecodeRequest };
    let val: &CFString = unsafe { kCGImageSourceDecodeToHDR };
    let opts = CFDictionary::<CFString, CFString>::from_slices(&[key], &[val]);
    let opts_ref: &CFDictionary = opts.as_opaque();
    let isrc = unsafe { CGImageSource::with_url(&url, Some(opts_ref)) }?;
    let index = unsafe { isrc.primary_image_index() };
    let image = unsafe { isrc.image_at_index(index, Some(opts_ref)) }?;
    let w = CGImage::width(Some(&image));
    let h = CGImage::height(Some(&image));
    Some((image, w, h))
}

/// Draw rows `[y0, y0 + rows)` into `buf` as 32-bit float RGBA in `cs`.
fn draw_band(
    image: &CGImage,
    cs: &CGColorSpace,
    w: usize,
    h: usize,
    y0: usize,
    rows: usize,
    buf: &mut [u8],
) {
    let stride = w * 16;
    buf[..stride * rows].fill(0);
    let bitmap = CGBitmapInfo(
        CGImageComponentInfo::Float.0
            | CGImageByteOrderInfo::Order32Little.0
            | CGImageAlphaInfo::PremultipliedLast.0,
    );
    let ctx = unsafe {
        CGBitmapContextCreate(
            buf.as_mut_ptr() as *mut c_void,
            w,
            rows,
            32,
            stride,
            Some(cs),
            bitmap.0,
        )
    }
    .expect("CGBitmapContextCreate");
    CGContext::draw_image(
        Some(&ctx),
        CGRect {
            origin: CGPoint { x: 0.0, y: (rows + y0) as f64 - h as f64 },
            size: CGSize { width: w as f64, height: h as f64 },
        },
        Some(image),
    );
}

/// Is this colour outside the gamut whose coordinates these are?
///
/// The tolerance has to be *relative*. An absolute one reports pixels as out of
/// gamut purely from float error in the two matrix multiplies, and the error
/// scales with the pixel: an HDR highlight at Y = 10 carries a thousand times
/// the absolute noise of a mid-tone. Measured symptom of getting this wrong —
/// 481 pixels of `IMG_4913` reported outside Rec.2020 while none were outside
/// P3, which cannot happen, since P3's primaries sit strictly inside
/// Rec.2020's.
fn out_of_gamut(v: [f64; 3]) -> bool {
    const REL: f64 = 1e-5;
    let peak = v.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let floor = -REL * peak.max(1.0);
    v.iter().any(|c| *c < floor)
}

fn px(buf: &[u8], i: usize) -> [f64; 3] {
    let o = i * 16;
    let c = |k: usize| {
        let b = &buf[o + k * 4..o + k * 4 + 4];
        f32::from_ne_bytes([b[0], b[1], b[2], b[3]]) as f64
    };
    [c(0), c(1), c(2)]
}

fn run(path: &Path) {
    let Some((image, w, h)) = open_image(path) else {
        println!("{}: ImageIO could not open it\n", path.display());
        return;
    };
    let cs709 = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceExtendedLinearSRGB })).unwrap();
    let csp3 =
        CGColorSpace::with_name(Some(unsafe { kCGColorSpaceExtendedLinearDisplayP3 })).unwrap();

    let band = 128.min(h);
    let stride = w * 16;
    let mut b709 = vec![0u8; stride * band];
    let mut bp3 = vec![0u8; stride * band];
    let mut c = Counts::default();
    let mut hist = Hist::new();

    let mut y0 = 0;
    while y0 < h {
        let rows = band.min(h - y0);
        draw_band(&image, &cs709, w, h, y0, rows, &mut b709);
        draw_band(&image, &csp3, w, h, y0, rows, &mut bp3);
        for i in 0..w * rows {
            let v709 = px(&b709, i);
            let vp3 = px(&bp3, i);
            c.total += 1;

            // The P3 render is the reference: converting it to 709 by matrix
            // gives the true 709 coordinates, negatives included, without
            // trusting the 709 render not to have been clipped.
            let xyz = apply(P3_TO_XYZ, vp3);
            let true709 = apply(XYZ_TO_RGB709, xyz);
            let disagree = (0..3)
                .map(|k| (true709[k] - v709[k]).abs())
                .fold(0.0f64, f64::max);
            if disagree > c.max_render_disagreement {
                c.max_render_disagreement = disagree;
            }

            if out_of_gamut(v709) {
                c.negative_in_709_render += 1;
            }
            let min709 = true709.iter().copied().fold(f64::INFINITY, f64::min);
            let peak = true709.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            let out709 = out_of_gamut(true709);
            if out709 {
                c.out_709_by_matrix += 1;
                if min709 < c.worst_negative {
                    c.worst_negative = min709;
                    c.worst_negative_relative = if peak > 0.0 { min709 / peak } else { 0.0 };
                }
            }
            if out_of_gamut(apply(XYZ_TO_P3, xyz)) {
                c.out_p3 += 1;
            }
            if out_of_gamut(apply(XYZ_TO_2020, xyz)) {
                c.out_2020 += 1;
            }
            if xyz[1] > 1.0 {
                c.above_white += 1;
            }

            // What the clamp costs, in CIEDE2000. Restricted to pixels at or
            // below diffuse white: above it L* exceeds 100 and Lab is outside
            // the range CIEDE2000 was fitted for, so a number there would look
            // authoritative without being meaningful.
            if out709 && xyz[1] <= 1.0 {
                let clamped = [true709[0].max(0.0), true709[1].max(0.0), true709[2].max(0.0)];
                let d = delta_e00(lab(xyz), lab(apply(RGB709_TO_XYZ, clamped)));
                hist.push(d);
            }
        }
        y0 += rows;
    }

    let pct = |n: u64| 100.0 * n as f64 / c.total.max(1) as f64;
    println!("{}", path.display());
    println!("  {w}x{h} = {:.2} MP", c.total as f64 / 1e6);
    println!(
        "  render cross-check: max |P3->709 matrix - 709 render| = {:.2e}  {}",
        c.max_render_disagreement,
        if c.max_render_disagreement < 1e-3 {
            "(the two renders agree: CoreGraphics preserved out-of-gamut colour)"
        } else {
            "(DISAGREE -- one render was clipped, treat the rest as void)"
        }
    );
    println!(
        "  negative component in the extended-linear-sRGB render: {:>10} px  {:>6.2}%",
        c.negative_in_709_render,
        pct(c.negative_in_709_render)
    );
    println!(
        "  outside Rec.709 : {:>10} px  {:>6.2}%",
        c.out_709_by_matrix,
        pct(c.out_709_by_matrix)
    );
    println!("  outside P3      : {:>10} px  {:>6.2}%", c.out_p3, pct(c.out_p3));
    println!("  outside Rec.2020: {:>10} px  {:>6.2}%", c.out_2020, pct(c.out_2020));
    println!(
        "  above diffuse white (Y > 1): {:>10} px  {:>6.2}%",
        c.above_white,
        pct(c.above_white)
    );
    println!(
        "  deepest negative: {:.4} ({:.1}% of that pixel's peak component)",
        c.worst_negative,
        100.0 * c.worst_negative_relative.abs()
    );
    if hist.n == 0 {
        println!("  CIEDE2000 of the clamp: nothing to measure (no out-of-gamut SDR pixels)\n");
        return;
    }
    println!(
        "  CIEDE2000 cost of the clamp, over the {} out-of-gamut pixels at or below white:",
        hist.n
    );
    println!(
        "    mean {:.2}  p50 {:.2}  p90 {:.2}  p99 {:.2}  max {:.2}",
        hist.mean(),
        hist.quantile(0.50),
        hist.quantile(0.90),
        hist.quantile(0.99),
        hist.max
    );
    println!(
        "    visible (dE >= 1.0): {:.1}% of them = {:.2}% of the image",
        100.0 * hist.fraction_above(1.0),
        pct((hist.fraction_above(1.0) * hist.n as f64) as u64)
    );
    println!(
        "    obvious (dE >= 3.0): {:.1}% of them = {:.2}% of the image\n",
        100.0 * hist.fraction_above(3.0),
        pct((hist.fraction_above(3.0) * hist.n as f64) as u64)
    );
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("usage: probe_gamut <image> [image...]");
        std::process::exit(2);
    }
    for a in &args {
        run(Path::new(a));
    }
}
