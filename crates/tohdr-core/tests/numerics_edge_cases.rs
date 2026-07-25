use tohdr_core::derive::{
    derive, derive_from_luma, linear_to_srgb, linear_to_srgb8, srgb8_to_linear, srgb_to_linear,
    DeriveOptions,
};
use tohdr_core::Rgb;

#[test]
fn linear_to_srgb8_never_panics_over_bit_pattern_sweep() {
    // Sweep a large chunk of the f32 bit-pattern space, including all
    // subnormals-adjacent and special values, and every bit pattern with a
    // stride to cover the whole range within test time.
    let specials: [f32; 7] = [
        -0.0f32,
        0.0f32,
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::NAN,
        f32::MIN_POSITIVE,
        -f32::MIN_POSITIVE,
    ];
    for &c in &specials {
        let _ = linear_to_srgb8(c); // must not panic / index OOB
    }
    // Full 32-bit sweep with stride, covering every exponent band including
    // subnormals (bit patterns near 0 and near 0x8000_0000). The assertion is
    // implicit: linear_to_srgb8 indexes a 16k table, so anything that escapes
    // its clamp panics here rather than in a 60 Mpx conversion.
    let mut i: u32 = 0;
    loop {
        let _ = linear_to_srgb8(f32::from_bits(i));
        if i == u32::MAX {
            break;
        }
        // stride to keep runtime sane: ~4M samples instead of 4B
        i = i.wrapping_add(1013);
        if i < 1013 {
            break;
        }
    }
}

#[test]
fn encode_table_signed_bias_report() {
    // Not just worst-case abs error (already tested) but *signed* mean error,
    // to catch a systematic +/- shift that would still pass a "within one
    // code" bound.
    let steps = 500_000;
    let mut sum_signed = 0f64;
    let mut n = 0u64;
    for i in 0..=steps {
        let lin = i as f32 / steps as f32;
        let exact = linear_to_srgb(lin) * 255.0; // continuous, unrounded
        let table = linear_to_srgb8(lin) as f64;
        sum_signed += table - exact as f64;
        n += 1;
    }
    let mean = sum_signed / n as f64;
    println!("encode table mean signed error (codes): {mean:.6}");
    // Report only; a well-built nearest-neighbor LUT should have mean close
    // to 0, not systematically biased by ~0.5.
    assert!(
        mean.abs() < 0.05,
        "systematic bias detected: mean signed error {mean:.6} codes"
    );
}

#[test]
fn decode_table_is_exact_not_merely_close() {
    // decode table claims to be exact (not "within one code"); verify tighter
    // than the existing 1e-7 test using a stricter bound tied to f32 ULPs.
    for code in 0u16..=255 {
        let want = srgb_to_linear(code as f32 / 255.0);
        let got = srgb8_to_linear(code as u8);
        assert_eq!(got, want, "code {code}: table and curve must be bit-identical");
    }
}

#[test]
fn powf_one_is_identity_for_all_f32_bit_patterns() {
    // unit_gamma fast path skips `t.powf(1.0)`. Confirm that is actually a
    // no-op for every float, including special values, or the fast path
    // changes output.
    let mut mismatches = 0u64;
    let mut worst: (f32, f32) = (0.0, 0.0);
    let mut i: u32 = 0;
    loop {
        let x = f32::from_bits(i);
        let p = x.powf(1.0);
        if p.to_bits() != x.to_bits() && !(x.is_nan() && p.is_nan()) {
            mismatches += 1;
            worst = (x, p);
        }
        if i == u32::MAX {
            break;
        }
        i = i.wrapping_add(997);
        if i < 997 {
            break;
        }
    }
    println!("powf(1.0) mismatches: {mismatches}, e.g. {worst:?}");
    assert_eq!(mismatches, 0, "powf(1.0) is not identity for some inputs: {worst:?}");
}

#[test]
fn all_non_finite_samples_yield_flat_zero_range_no_panic() {
    // Construct an "hdr"/"base" luma pair where every sample is NaN or inf,
    // via derive_from_luma directly (bypassing Rgb's u16 storage, which can't
    // hold NaN anyway) to hit the "no finite sample at all" branch.
    let w = 4;
    let h = 4;
    let opts = DeriveOptions::default();
    let (plane, meta) = derive_from_luma(
        w,
        h,
        |_, _| f32::INFINITY,
        |_, _| f32::NAN,
        &opts,
    );
    assert_eq!(meta.min_log2[0], 0.0);
    assert_eq!(meta.max_log2[0], 0.0);
    assert!(plane.data.iter().all(|&v| v == 0), "flat range must encode as 0");
}

#[test]
fn exactly_one_finite_sample_sets_the_whole_range() {
    let w = 4;
    let h = 4;
    let opts = DeriveOptions::default();
    // pixel (0,0) is the only finite one; everything else NaN/inf.
    let (plane, meta) = derive_from_luma(
        w,
        h,
        move |x, y| if x == 0 && y == 0 { 2.0 } else { f32::NAN },
        move |_x, _y| 1.0,
        &opts,
    );
    println!("one-finite: min={} max={}", meta.min_log2[0], meta.max_log2[0]);
    assert!(meta.min_log2[0].is_finite());
    assert!(meta.max_log2[0].is_finite());
    assert_eq!(meta.min_log2[0], meta.max_log2[0], "single finite sample => zero range");
    let _ = plane;
}

#[test]
fn all_zero_image_derive_does_not_panic_and_is_flat() {
    let w = 6;
    let h = 6;
    let n = (w * h * 3) as usize;
    let hdr = Rgb { width: w, height: h, bits: 8, data: vec![0u16; n] };
    let base = Rgb { width: w, height: h, bits: 8, data: vec![0u16; n] };
    let opts = DeriveOptions::default();
    let (plane, meta) = derive(&hdr, &base, &opts);
    println!("all-zero: min={} max={}", meta.min_log2[0], meta.max_log2[0]);
    assert!(plane.data.iter().all(|&v| v == 0));
}

#[test]
fn parallel_derive_matches_single_threaded_serial_reimplementation() {
    // Build a non-trivial pseudo-random image, run the real (parallel) derive,
    // and compare bit-for-bit against a serial re-derivation of the log2_gain
    // step done in strict row-major order with a single accumulator, to check
    // the parallel decomposition didn't reorder floating-point operations that
    // matter. Since each log2_gain cell is computed independently (no cross-
    // element accumulation) this should trivially match; the real risk is in
    // the gain-plane averaging sum order across a subsample block.
    let w = 37u32; // deliberately not a multiple of thread count / subsample
    let h = 29u32;
    let n = (w * h * 3) as usize;
    let mut hdr_data = vec![0u16; n];
    let mut base_data = vec![0u16; n];
    let mut state = 0x2545F4914F6CDD1Du64;
    let mut rnd = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state % 256) as u16
    };
    for i in 0..n {
        hdr_data[i] = rnd();
        base_data[i] = rnd();
    }
    let hdr = Rgb { width: w, height: h, bits: 8, data: hdr_data };
    let base = Rgb { width: w, height: h, bits: 8, data: base_data };
    let mut opts = DeriveOptions::default();
    opts.subsample = 2; // exercise the subsample-average path, non-divisible dims
    let (plane_par, meta_par) = derive(&hdr, &base, &opts);

    // Serial reimplementation of the exact same math, single-threaded,
    // strict row-major summation order matching what for_each_out_row_chunk_mut
    // does with one worker.
    let (plane_serial, meta_serial) = serial_reference_derive(&hdr, &base, &opts);

    assert_eq!(meta_par.min_log2[0], meta_serial.min_log2[0]);
    assert_eq!(meta_par.max_log2[0], meta_serial.max_log2[0]);
    assert_eq!(plane_par.width, plane_serial.width);
    assert_eq!(plane_par.height, plane_serial.height);
    for (i, (&a, &b)) in plane_par.data.iter().zip(plane_serial.data.iter()).enumerate() {
        assert_eq!(a, b, "gain plane cell {i} diverges between parallel and serial");
    }
}

/// Mirrors derive_from_luma's math exactly, but with a single-threaded,
/// strictly ordered accumulation, to serve as an oracle for bit-identity.
fn serial_reference_derive(
    hdr: &Rgb,
    base: &Rgb,
    opts: &DeriveOptions,
) -> (tohdr_core::GainPlane, tohdr_core::GainMapMeta) {
    let w = hdr.width;
    let h = hdr.height;
    let n = w as usize * h as usize;
    let mut log2_gain = vec![0f32; n];
    let luma_of = |img: &Rgb, x: u32, y: u32| {
        let idx = (y as usize * img.width as usize + x as usize) * 3;
        let mv = img.max_value() as f32;
        let r = srgb_to_linear(img.data[idx] as f32 / mv);
        let g = srgb_to_linear(img.data[idx + 1] as f32 / mv);
        let b = srgb_to_linear(img.data[idx + 2] as f32 / mv);
        0.2126 * r + 0.7152 * g + 0.0722 * b
    };
    for y in 0..h {
        for x in 0..w {
            let base_l = luma_of(base, x, y);
            let alt_l = luma_of(hdr, x, y);
            // Must mirror derive_from_luma exactly, NaN branch included, or
            // this oracle stops being one.
            let ratio = (alt_l + opts.alt_offset) / (base_l + opts.base_offset);
            log2_gain[y as usize * w as usize + x as usize] =
                if ratio.is_nan() { f32::NAN } else { ratio.max(1e-6).log2() };
        }
    }
    let mut min_log2 = f32::INFINITY;
    let mut max_log2 = f32::NEG_INFINITY;
    for &v in &log2_gain {
        if v.is_finite() {
            min_log2 = min_log2.min(v);
            max_log2 = max_log2.max(v);
        }
    }
    if !min_log2.is_finite() || !max_log2.is_finite() {
        min_log2 = 0.0;
        max_log2 = 0.0;
    }
    let range = (max_log2 - min_log2).max(0.0);
    let subsample = opts.subsample.max(1);
    let gw = w.div_ceil(subsample);
    let gh = h.div_ceil(subsample);
    let mut data = vec![0u8; gw as usize * gh as usize];
    let gamma = opts.gamma;
    let unit_gamma = (gamma - 1.0).abs() < 1e-6;
    for gy in 0..gh {
        let y0 = gy * subsample;
        let y1 = (y0 + subsample).min(h);
        for gx in 0..gw {
            let x0 = gx * subsample;
            let x1 = (x0 + subsample).min(w);
            let mut sum = 0f32;
            let mut count = 0u32;
            for y in y0..y1 {
                let row = y as usize * w as usize;
                for x in x0..x1 {
                    let v = log2_gain[row + x as usize];
                    let v = if v.is_nan() { min_log2 } else { v.clamp(min_log2, max_log2) };
                    let norm = if range > 0.0 {
                        let t = ((v - min_log2) / range).clamp(0.0, 1.0);
                        if unit_gamma { t } else { t.powf(gamma) }
                    } else {
                        0.0
                    };
                    sum += norm;
                    count += 1;
                }
            }
            let avg = if count > 0 { sum / count as f32 } else { 0.0 };
            data[gy as usize * gw as usize + gx as usize] = (avg.clamp(0.0, 1.0) * 255.0).round() as u8;
        }
    }
    let meta = tohdr_core::GainMapMeta {
        min_log2: [min_log2; 3],
        max_log2: [max_log2; 3],
        gamma: [opts.gamma; 3],
        base_offset: [opts.base_offset; 3],
        alt_offset: [opts.alt_offset; 3],
        base_headroom: 0.0,
        alt_headroom: opts.alt_headroom,
        use_base_color_space: true,
    };
    (tohdr_core::GainPlane { width: gw, height: gh, data }, meta)
}
