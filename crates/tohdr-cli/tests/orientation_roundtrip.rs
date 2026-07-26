//! A rotated source must come out the same way up, through either engine.
//!
//! This test owns an integration boundary on purpose: the question it answers —
//! *does `irot`/`imir` mean what we think it means* — has no answer inside this
//! codebase. `tohdr_core::orient`'s unit tests prove the coordinate algebra but
//! have to assume a reading of `imir`'s `axis` field, and that assumption was
//! wrong for four of the eight orientations until ImageIO said so. So ImageIO is
//! the oracle, and it has to be the real one.
//!
//! `examples/probe_orientation.rs` prints the same measurement as a table when
//! something here fails and the per-value detail is wanted.

use tohdr_core::{
    heif_transform, EncodeOptions, Flavor, GainMapEncoder, GainMapMeta, GainPlane, Rgb,
};

/// Small, and a gradient rather than a flat fill — a uniform image would survive
/// any transform, right or wrong, so it could not fail this test.
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
        Rgb { width: W, height: H, bits: 8, data },
        GainPlane {
            width: W / 2,
            height: H / 2,
            data: vec![128u8; (W / 2 * H / 2) as usize],
        },
        GainMapMeta::default(),
    )
}

fn opts_for(orientation: u8) -> EncodeOptions<'static> {
    EncodeOptions {
        flavor: Flavor::Both,
        orientation: heif_transform(orientation),
        ..EncodeOptions::default()
    }
}

/// ImageIO omits the property for an upright image, which is the same statement
/// as `1`.
fn read_back(bytes: &[u8]) -> u32 {
    tohdr_apple::inspect_bytes(bytes)
        .expect("ImageIO reads our own output")
        .orientation
        .unwrap_or(1)
}

#[test]
fn engine_b_round_trips_every_exif_orientation() {
    let (base, gain, meta) = inputs();
    for want in 1u8..=8 {
        let bytes = tohdr_portable::PortableEngine
            .encode(&base, &gain, &meta, &opts_for(want))
            .expect("encode");
        assert_eq!(
            read_back(&bytes),
            want as u32,
            "orientation {want} came back wrong from our own muxer: {:?}",
            heif_transform(want)
        );
    }
}

/// Engine A takes the orientation as a number and lets ImageIO write the boxes,
/// so this pins the *handoff* rather than the box semantics — that the number
/// reaches the destination at all, which it does not if it is only present inside
/// the carried TIFF dictionary.
#[test]
fn engine_a_round_trips_every_exif_orientation() {
    let (base, gain, meta) = inputs();
    for want in 1u8..=8 {
        let bytes = tohdr_apple::AppleEngine
            .encode(&base, &gain, &meta, &opts_for(want))
            .expect("encode");
        assert_eq!(read_back(&bytes), want as u32, "orientation {want}");
    }
}

/// The two engines must agree, since a caller picks one for reasons unrelated to
/// which way up the photograph is.
#[test]
fn the_two_engines_state_the_same_transform() {
    let (base, gain, meta) = inputs();
    for want in 1u8..=8 {
        let a = tohdr_apple::AppleEngine
            .encode(&base, &gain, &meta, &opts_for(want))
            .expect("engine A");
        let b = tohdr_portable::PortableEngine
            .encode(&base, &gain, &meta, &opts_for(want))
            .expect("engine B");
        assert_eq!(read_back(&a), read_back(&b), "orientation {want}");
    }
}

/// An upright source must not gain a transform it did not ask for — the case
/// every existing file exercises, and the one a regression here would hit first.
#[test]
fn an_upright_source_stays_untransformed() {
    let (base, gain, meta) = inputs();
    for engine_name in ["a", "b"] {
        let bytes = if engine_name == "a" {
            tohdr_apple::AppleEngine
                .encode(&base, &gain, &meta, &EncodeOptions::default())
                .expect("engine A")
        } else {
            tohdr_portable::PortableEngine
                .encode(&base, &gain, &meta, &EncodeOptions::default())
                .expect("engine B")
        };
        assert_eq!(read_back(&bytes), 1, "engine {engine_name}");
    }
}
