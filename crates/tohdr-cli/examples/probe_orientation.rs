//! Does a container transform we write mean what we meant?
//!
//! `tohdr_core::orient`'s own tests prove the *algebra* — that the `irot`/`imir`
//! pair we pick composes to the coordinate map Exif defines — but they have to
//! assume the order the two boxes are applied in, and a spec sentence is not a
//! measurement. This asks the only authority that matters on this platform:
//! write all eight orientations through both engines, and have ImageIO resolve
//! each file's boxes back into an Exif orientation number.
//!
//! A row where `read back` differs from `exif in` is a photo that comes out the
//! wrong way up in Photos, Preview and every app built on ImageIO.
//!
//! ```text
//! cargo run --release -p tohdr-cli --example probe_orientation
//! ```

use tohdr_core::{
    heif_transform, EncodeOptions, Flavor, GainMapEncoder, GainMapMeta, GainPlane, Rgb,
};

/// A gradient rather than a flat fill: a flat image would round-trip through any
/// transform, correct or not.
fn inputs() -> (Rgb, GainPlane, GainMapMeta) {
    const W: u32 = 64;
    const H: u32 = 32;
    let mut data = Vec::with_capacity((W * H * 3) as usize);
    for y in 0..H {
        for x in 0..W {
            data.push((x * 4) as u16);
            data.push((y * 8) as u16);
            data.push(128);
        }
    }
    (
        Rgb {
            width: W,
            height: H,
            bits: 8,
            data,
        },
        GainPlane {
            width: W / 2,
            height: H / 2,
            data: vec![128u8; (W / 2 * H / 2) as usize],
        },
        GainMapMeta::default(),
    )
}

fn main() {
    let (base, gain, meta) = inputs();
    println!("exif in   engine            irot  imir   read back   verdict");
    let mut bad = 0;

    for exif in 1u8..=8 {
        let t = heif_transform(exif);
        let opts = EncodeOptions {
            flavor: Flavor::Both,
            orientation: t,
            ..EncodeOptions::default()
        };
        for (name, bytes) in [
            (
                "portable-hpvca",
                tohdr_portable::PortableEngine
                    .encode(&base, &gain, &meta, &opts)
                    .map_err(|e| format!("{e}")),
            ),
            (
                "apple-imageio",
                tohdr_apple::AppleEngine
                    .encode(&base, &gain, &meta, &opts)
                    .map_err(|e| format!("{e:?}")),
            ),
        ] {
            let mirror = t.mirror_axis.map_or("-".to_string(), |a| a.to_string());
            match bytes.and_then(|b| tohdr_apple::inspect_bytes(&b).map_err(|e| format!("{e}"))) {
                Ok(rb) => {
                    // ImageIO omits the property for an upright image, which is
                    // the same statement as `1`.
                    let got = rb.orientation.unwrap_or(1);
                    let ok = got == exif as u32;
                    bad += !ok as u32;
                    println!(
                        "{exif:>7}   {name:<16}  {:>4}  {mirror:>4}   {got:>9}   {}",
                        t.rotate_ccw_quarters,
                        if ok { "ok" } else { "MISMATCH" }
                    );
                }
                Err(e) => {
                    bad += 1;
                    println!("{exif:>7}   {name:<16}  {:>4}  {mirror:>4}   {:>9}   ERROR: {e}", t.rotate_ccw_quarters, "-");
                }
            }
        }
    }

    println!();
    if bad == 0 {
        println!("all 16 files read back as the orientation their source stated");
    } else {
        println!("{bad} of 16 disagree — the mapping or the application order is wrong");
        std::process::exit(1);
    }
}
