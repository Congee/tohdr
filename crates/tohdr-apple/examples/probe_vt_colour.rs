//! Why is the hardware path stuck at 49 dB no matter the bitrate?
//!
//! `probe_vt_quality.rs` found the reconstruction PSNR of the VideoToolbox path
//! pinned at 49.04 dB from q85 to q100 while the file grew 7.5x. Quantization
//! error responds to bitrate; that does not. So the loss is systematic, and the
//! obvious candidate is the `colr` box: we hand VideoToolbox BGRA and let *it*
//! choose an RGB→YCbCr matrix, then declare a matrix of our own in the container.
//! If the two disagree, every pixel decodes through the wrong inverse matrix — a
//! constant error, exactly what the sweep shows.
//!
//! This tries the plausible declarations against the metric. Whichever one the
//! encoder actually used should stand out, not marginally but by tens of dB.
//!
//! Run: `cargo run --release --example probe_vt_colour -p tohdr-apple -- <hdr.tiff>`

use std::path::{Path, PathBuf};

use tohdr_apple::vtenc;
use tohdr_core::derive::DeriveOptions;
use tohdr_core::hdr::{derive_consistent, ToneMap};
use tohdr_core::{EncodeOptions, HdrRgb};
use tohdr_heif::{CodedImage, ColourInfo, MuxRequest};

fn compare(src: &HdrRgb, got: &HdrRgb) -> Option<(f64, f64)> {
    if src.width != got.width || src.height != got.height {
        return None;
    }
    let mut sse = 0.0f64;
    let mut n = 0usize;
    let mut peak = 0.0f64;
    let mut rels: Vec<f64> = Vec::new();
    for y in 0..src.height {
        for x in 0..src.width {
            let a = src.luma(x, y) as f64;
            peak = peak.max(a);
            if a < 0.05 {
                continue;
            }
            let b = got.luma(x, y) as f64;
            rels.push(((b - a) / a).abs());
            sse += (b - a) * (b - a);
            n += 1;
        }
    }
    if n == 0 {
        return None;
    }
    rels.sort_by(|p, q| p.partial_cmp(q).unwrap());
    let p999 = rels[((rels.len() as f64 * 0.999) as usize).min(rels.len() - 1)];
    Some((20.0 * (peak / (sse / n as f64).sqrt()).log10(), p999))
}

fn coded(plane: vtenc::CodedPlane) -> CodedImage {
    CodedImage {
        width: plane.width,
        height: plane.height,
        bit_depth: 8,
        chroma: tohdr_heif::chroma_for(plane.monochrome),
        hvcc: plane.hvcc,
        data: plane.data,
    }
}

fn main() {
    let src_path = PathBuf::from(
        std::env::args()
            .nth(1)
            .expect("usage: probe_vt_colour <hdr.tiff>"),
    );
    let hdr = tohdr_portable::load_hdr(&src_path).expect("load source");
    let white = hdr.peak_luma(0.001);
    let base = ToneMap::Reinhard { white }.to_sdr(&hdr);
    let (gain, meta) = derive_consistent(&hdr, &base, &DeriveOptions::default());
    let opts = EncodeOptions::default();

    // One encode per codec, reused for every declaration: the coded bytes are
    // identical across a codec's rows, only the `colr` differs, which is the
    // whole point. The software codec is swept too — which matrix *it* writes is
    // equally an assumption until measured, and the two need not agree.
    let hw = (
        "videotoolbox",
        coded(vtenc::encode_base_tuned(&base, 95, false).expect("base")),
        coded(vtenc::encode_gain_tuned(&gain, 95, false).expect("gain")),
    );
    let sw_codec = tohdr_portable::HpvcaCodec;
    let sw = (
        "hpvca",
        tohdr_heif::PlaneCodec::encode_base(&sw_codec, &base, 95).expect("sw base"),
        tohdr_heif::PlaneCodec::encode_gain(&sw_codec, &gain, 95).expect("sw gain"),
    );

    println!(
        "source {}x{} ({:.2} MP), one encode per codec, varying only the declared colr\n",
        hdr.width,
        hdr.height,
        (hdr.width as f64 * hdr.height as f64) / 1e6
    );

    for (label, bp, gp) in [hw, sw] {
        println!("  {label}:");
        println!("    matrix  transfer  range   PSNR      p99.9");
        sweep(&hdr, &bp, &gp, meta, &opts);
        println!();
    }
}

fn sweep(
    hdr: &HdrRgb,
    bp: &CodedImage,
    gp: &CodedImage,
    meta: tohdr_core::GainMapMeta,
    opts: &EncodeOptions,
) {
    // matrix: 1 = BT.709, 5 = BT.470BG, 6 = BT.601/SMPTE170M.
    // transfer: 13 = sRGB, 1 = BT.709.
    for &(matrix, transfer) in &[(1u16, 13u16), (6, 13), (5, 13), (1, 1), (6, 1)] {
        for full_range in [true, false] {
            let req = MuxRequest {
                base: bp.clone(),
                gain: gp.clone(),
                meta,
                flavor: opts.flavor,
                base_colour: Some(ColourInfo::Nclx {
                    primaries: 1,
                    transfer,
                    matrix,
                    full_range,
                }),
                tmap_colour: Some(ColourInfo::Nclx {
                    primaries: 12,
                    transfer: 16,
                    matrix: 6,
                    full_range: true,
                }),
                exif: None,
                xmp: None,
                clli: None,
            };
            let bytes = tohdr_heif::mux(&req).expect("mux");
            let path = Path::new("out/vtc_probe.heic");
            std::fs::write(path, &bytes).expect("write");
            match tohdr_apple::load_hdr(path).ok().and_then(|d| compare(hdr, &d)) {
                Some((psnr, p999)) => println!(
                    "    {matrix:^6}  {transfer:^8}  {:^5}   {psnr:6.2} dB  {:.2}%",
                    if full_range { "full" } else { "vid" },
                    p999 * 100.0
                ),
                None => println!(
                    "    {matrix:^6}  {transfer:^8}  {:^5}   decode failed",
                    full_range
                ),
            }
        }
    }
}
