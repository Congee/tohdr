//! `tohdr inspect`: report what ImageIO actually sees in a file's gain map,
//! both flavors. Built on [`tohdr_apple::inspect`] — Apple ImageIO is the
//! project's correctness oracle (see `tohdr-apple`'s crate docs), so this is
//! "ground truth", not a guess from our own container parser.

use anyhow::Context;
use serde::Serialize;
use tohdr_apple::ReadBack;

use crate::cli::InspectArgs;
use crate::panic_guard::catch;

#[derive(Serialize, Debug)]
struct IsoMetaJson {
    min_log2: f32,
    max_log2: f32,
    gamma: f32,
    base_offset: f32,
    alt_offset: f32,
    base_headroom: f32,
    alt_headroom: f32,
    headroom_consistent: bool,
}

#[derive(Serialize, Debug)]
struct InspectJson {
    file: String,
    width: u32,
    height: u32,
    depth: u32,
    apple_gain_map: bool,
    iso_gain_map: bool,
    gain_plane_size: Option<(u32, u32)>,
    gain_pixel_format: Option<String>,
    maker_apple_tag33: Option<f64>,
    maker_apple_tag48: Option<f64>,
    apple_headroom_stops: Option<f32>,
    iso_meta: Option<IsoMetaJson>,
}

fn fourcc(code: u32) -> String {
    let b = code.to_be_bytes();
    String::from_utf8_lossy(&b).into_owned()
}

fn to_json(file: &str, rb: &ReadBack) -> InspectJson {
    let iso_meta = rb.iso_meta.as_ref().map(|m| IsoMetaJson {
        min_log2: m.min_log2[0],
        max_log2: m.max_log2[0],
        gamma: m.gamma[0],
        base_offset: m.base_offset[0],
        alt_offset: m.alt_offset[0],
        base_headroom: m.base_headroom,
        alt_headroom: m.alt_headroom,
        headroom_consistent: rb.headroom_consistent().unwrap_or(false),
    });
    InspectJson {
        file: file.to_string(),
        width: rb.width,
        height: rb.height,
        depth: rb.depth,
        apple_gain_map: rb.apple_aux,
        iso_gain_map: rb.iso_aux,
        gain_plane_size: rb.gain_size,
        gain_pixel_format: rb.gain_pixel_format.map(fourcc),
        maker_apple_tag33: rb.tag33,
        maker_apple_tag48: rb.tag48,
        apple_headroom_stops: rb.apple_headroom,
        iso_meta,
    }
}

fn print_human(j: &InspectJson) {
    println!("{}", j.file);
    println!("  dimensions: {}x{} ({}-bit)", j.width, j.height, j.depth);
    println!(
        "  apple gain map: {}    iso gain map: {}",
        j.apple_gain_map, j.iso_gain_map
    );
    if let Some((w, h)) = j.gain_plane_size {
        println!(
            "  gain plane: {w}x{h}, format {}",
            j.gain_pixel_format.as_deref().unwrap_or("?")
        );
    } else {
        println!("  gain plane: none");
    }
    match (j.maker_apple_tag33, j.maker_apple_tag48) {
        (Some(t33), Some(t48)) => println!(
            "  MakerApple tag33={t33:.6} tag48={t48:.6} -> headroom {}",
            j.apple_headroom_stops
                .map(|h| format!("{h:.3}x"))
                .unwrap_or_else(|| "?".into())
        ),
        _ => println!("  MakerApple tags: absent"),
    }
    if let Some(m) = &j.iso_meta {
        println!(
            "  iso meta: min_log2={:.3} max_log2={:.3} alt_headroom={:.3} (consistent: {})",
            m.min_log2, m.max_log2, m.alt_headroom, m.headroom_consistent
        );
    } else {
        println!("  iso meta: absent");
    }
}

pub fn run(args: InspectArgs) -> anyhow::Result<i32> {
    let path = args.file.as_path();
    eprintln!("tohdr: inspecting {} via apple-imageio", path.display());
    let rb = catch("apple-imageio", "inspect", || tohdr_apple::inspect(path))
        .with_context(|| format!("inspecting {}", path.display()))?;

    let j = to_json(&path.display().to_string(), &rb);
    if args.json {
        println!("{}", serde_json::to_string(&j)?);
    } else {
        print_human(&j);
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tohdr_core::GainMapMeta;

    fn sample_readback() -> ReadBack {
        ReadBack {
            width: 100,
            height: 80,
            depth: 8,
            apple_aux: true,
            iso_aux: true,
            gain_size: Some((50, 40)),
            gain_pixel_format: Some(u32::from_be_bytes(*b"L008")),
            tag33: Some(1.0),
            tag48: Some(0.05),
            apple_headroom: Some(4.88),
            iso_meta: Some(GainMapMeta::with_headroom_stops(2.287109)),
        }
    }

    #[test]
    fn fourcc_roundtrips_ascii() {
        assert_eq!(fourcc(u32::from_be_bytes(*b"L008")), "L008");
    }

    #[test]
    fn json_shape_reflects_readback() {
        let rb = sample_readback();
        let j = to_json("f.heic", &rb);
        assert_eq!(j.file, "f.heic");
        assert!(j.apple_gain_map && j.iso_gain_map);
        assert_eq!(j.gain_plane_size, Some((50, 40)));
        assert_eq!(j.gain_pixel_format.as_deref(), Some("L008"));
        let m = j.iso_meta.expect("iso meta present");
        assert!(m.headroom_consistent, "with_headroom_stops keeps max_log2 == alt_headroom");
    }

    #[test]
    fn absent_iso_meta_serializes_as_none() {
        let mut rb = sample_readback();
        rb.iso_meta = None;
        let j = to_json("f.heic", &rb);
        assert!(j.iso_meta.is_none());
    }
}
