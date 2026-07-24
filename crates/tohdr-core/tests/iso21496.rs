//! Round-trip + cross-implementation checks for `tohdr_core::iso21496`.
//!
//! The oracle is `ultrahdr-core` 0.6 (dev-dependency only). Its
//! `parse_iso21496_fmt`/`serialize_iso21496_fmt` are re-exports of the
//! `zencodec` crate's implementation of the same clause C.2.2 layout; see
//! `zencodec-0.1.23/src/gainmap.rs:99-121` for confirmation that its
//! `Iso21496Format::JxlJhgm` variant is byte-for-byte the same "bare payload"
//! shape our `serialize`/`parse` implement (no AVIF version-byte prefix, no
//! URN). Field types there (`zencodec::GainMapParams`/`GainMapChannel`) use
//! continued-fraction encoding rather than our fixed 1/65536 denominator, so
//! bytes differ for the same value — hence field-level comparisons below,
//! not byte-for-byte ones.

use tohdr_core::iso21496;
use tohdr_core::GainMapMeta;

use ultrahdr_core::{parse_iso21496_fmt, serialize_iso21496_fmt, GainMapChannel, Iso21496Format};

const TOL: f32 = 5e-4;

fn assert_meta_close(a: &GainMapMeta, b: &GainMapMeta) {
    for c in 0..3 {
        assert!(
            (a.min_log2[c] - b.min_log2[c]).abs() < TOL,
            "min_log2[{c}]: {} vs {}",
            a.min_log2[c],
            b.min_log2[c]
        );
        assert!(
            (a.max_log2[c] - b.max_log2[c]).abs() < TOL,
            "max_log2[{c}]: {} vs {}",
            a.max_log2[c],
            b.max_log2[c]
        );
        assert!(
            (a.gamma[c] - b.gamma[c]).abs() < TOL,
            "gamma[{c}]: {} vs {}",
            a.gamma[c],
            b.gamma[c]
        );
        assert!(
            (a.base_offset[c] - b.base_offset[c]).abs() < TOL,
            "base_offset[{c}]: {} vs {}",
            a.base_offset[c],
            b.base_offset[c]
        );
        assert!(
            (a.alt_offset[c] - b.alt_offset[c]).abs() < TOL,
            "alt_offset[{c}]: {} vs {}",
            a.alt_offset[c],
            b.alt_offset[c]
        );
    }
    assert!((a.base_headroom - b.base_headroom).abs() < TOL);
    assert!((a.alt_headroom - b.alt_headroom).abs() < TOL);
    assert_eq!(a.use_base_color_space, b.use_base_color_space);
}

/// Test metas: SDR base, HDR base, monochrome, per-channel-differing, gamma
/// != 1, negative min_log2.
fn test_metas() -> Vec<GainMapMeta> {
    vec![
        // SDR base, default-ish.
        GainMapMeta {
            min_log2: [0.0; 3],
            max_log2: [2.0; 3],
            gamma: [1.0; 3],
            base_offset: [1.0 / 64.0; 3],
            alt_offset: [1.0 / 64.0; 3],
            base_headroom: 0.0,
            alt_headroom: 2.0,
            use_base_color_space: true,
        },
        // HDR base (base_headroom > 0), alt is SDR-ish.
        GainMapMeta {
            min_log2: [-1.5; 3],
            max_log2: [0.0; 3],
            gamma: [1.0; 3],
            base_offset: [1.0 / 64.0; 3],
            alt_offset: [1.0 / 64.0; 3],
            base_headroom: 3.0,
            alt_headroom: 0.0,
            use_base_color_space: false,
        },
        // Monochrome (all channels identical, non-default values).
        GainMapMeta {
            min_log2: [-0.25; 3],
            max_log2: [3.5; 3],
            gamma: [1.0; 3],
            base_offset: [0.01; 3],
            alt_offset: [0.02; 3],
            base_headroom: 0.0,
            alt_headroom: 1.75,
            use_base_color_space: true,
        },
        // Per-channel differing (forces multichannel encoding).
        GainMapMeta {
            min_log2: [0.0, -0.5, 0.25],
            max_log2: [2.0, 2.5, 3.0],
            gamma: [1.0, 1.2, 0.8],
            base_offset: [1.0 / 64.0, 1.0 / 32.0, 1.0 / 128.0],
            alt_offset: [1.0 / 64.0, 1.0 / 16.0, 1.0 / 256.0],
            base_headroom: 0.0,
            alt_headroom: 2.3,
            use_base_color_space: true,
        },
        // Gamma != 1.
        GainMapMeta {
            min_log2: [0.0; 3],
            max_log2: [4.0; 3],
            gamma: [2.2; 3],
            base_offset: [1.0 / 64.0; 3],
            alt_offset: [1.0 / 64.0; 3],
            base_headroom: 0.0,
            alt_headroom: 4.0,
            use_base_color_space: true,
        },
        // Negative min_log2.
        GainMapMeta {
            min_log2: [-3.25; 3],
            max_log2: [1.0; 3],
            gamma: [1.0; 3],
            base_offset: [1.0 / 64.0; 3],
            alt_offset: [1.0 / 64.0; 3],
            base_headroom: 0.5,
            alt_headroom: 1.0,
            use_base_color_space: true,
        },
    ]
}

#[test]
fn round_trip_within_tolerance() {
    for meta in test_metas() {
        let bytes = iso21496::serialize(&meta);
        let parsed = iso21496::parse(&bytes).expect("parse should succeed");
        assert_meta_close(&meta, &parsed);
    }
}

/// Known-vector test: every value below is exactly representable at our
/// fixed denominator (1/64 = 1024/65536), so serialize() must produce exact
/// expected bytes, not just round-trip approximately.
#[test]
fn serialize_matches_expected_bytes_for_exact_values() {
    let meta = GainMapMeta {
        min_log2: [0.0; 3],
        max_log2: [2.0; 3],
        gamma: [1.0; 3],
        base_offset: [1.0 / 64.0; 3],
        alt_offset: [1.0 / 64.0; 3],
        base_headroom: 0.0,
        alt_headroom: 2.0,
        use_base_color_space: true,
    };
    let bytes = iso21496::serialize(&meta);

    let mut expected = Vec::new();
    expected.extend_from_slice(&0u16.to_be_bytes()); // minimum_version
    expected.extend_from_slice(&0u16.to_be_bytes()); // writer_version
    expected.push(0x40); // is_multichannel=0, use_base_colour_space=1, reserved=0
    expected.extend_from_slice(&0u32.to_be_bytes()); // base_headroom num = 0
    expected.extend_from_slice(&65536u32.to_be_bytes()); // base_headroom den
    expected.extend_from_slice(&131072u32.to_be_bytes()); // alt_headroom num = 2 * 65536
    expected.extend_from_slice(&65536u32.to_be_bytes()); // alt_headroom den
    expected.extend_from_slice(&0i32.to_be_bytes()); // min_log2 num = 0
    expected.extend_from_slice(&65536u32.to_be_bytes());
    expected.extend_from_slice(&131072i32.to_be_bytes()); // max_log2 num = 2 * 65536
    expected.extend_from_slice(&65536u32.to_be_bytes());
    expected.extend_from_slice(&65536u32.to_be_bytes()); // gamma num = 1 * 65536
    expected.extend_from_slice(&65536u32.to_be_bytes());
    expected.extend_from_slice(&1024i32.to_be_bytes()); // base_offset num = 65536/64
    expected.extend_from_slice(&65536u32.to_be_bytes());
    expected.extend_from_slice(&1024i32.to_be_bytes()); // alt_offset num = 65536/64
    expected.extend_from_slice(&65536u32.to_be_bytes());

    assert_eq!(bytes, expected);
    assert_eq!(bytes.len(), 4 + 1 + 16 + 40); // matches avifGainMapMetadataSize for channel_count=1
}

#[test]
fn parse_rejects_unsupported_minimum_version() {
    let mut bytes = iso21496::serialize(&GainMapMeta::default());
    bytes[0] = 0;
    bytes[1] = 1; // minimum_version = 1
    let err = iso21496::parse(&bytes).unwrap_err();
    assert_eq!(err, iso21496::ParseError::UnsupportedVersion(1));
}

#[test]
fn parse_rejects_truncated_input() {
    let bytes = iso21496::serialize(&GainMapMeta::default());
    let err = iso21496::parse(&bytes[..bytes.len() - 1]).unwrap_err();
    assert_eq!(err, iso21496::ParseError::Truncated);
}

fn to_oracle_channel(meta: &GainMapMeta, c: usize) -> GainMapChannel {
    GainMapChannel {
        min: meta.min_log2[c] as f64,
        max: meta.max_log2[c] as f64,
        gamma: meta.gamma[c] as f64,
        base_offset: meta.base_offset[c] as f64,
        alternate_offset: meta.alt_offset[c] as f64,
    }
}

fn to_oracle(meta: &GainMapMeta) -> ultrahdr_core::GainMapMetadata {
    // `GainMapParams` is `#[non_exhaustive]`: build via `Default` + field
    // assignment rather than a full struct literal.
    let mut p = ultrahdr_core::GainMapMetadata::default();
    p.channels = [
        to_oracle_channel(meta, 0),
        to_oracle_channel(meta, 1),
        to_oracle_channel(meta, 2),
    ];
    p.base_hdr_headroom = meta.base_headroom as f64;
    p.alternate_hdr_headroom = meta.alt_headroom as f64;
    p.use_base_color_space = meta.use_base_color_space;
    p.backward_direction = false;
    p
}

fn from_oracle_channel(c: &GainMapChannel) -> (f32, f32, f32, f32, f32) {
    (
        c.min as f32,
        c.max as f32,
        c.gamma as f32,
        c.base_offset as f32,
        c.alternate_offset as f32,
    )
}

fn from_oracle(p: &ultrahdr_core::GainMapMetadata) -> GainMapMeta {
    let mut min_log2 = [0.0f32; 3];
    let mut max_log2 = [0.0f32; 3];
    let mut gamma = [0.0f32; 3];
    let mut base_offset = [0.0f32; 3];
    let mut alt_offset = [0.0f32; 3];
    for c in 0..3 {
        let (mn, mx, g, bo, ao) = from_oracle_channel(&p.channels[c]);
        min_log2[c] = mn;
        max_log2[c] = mx;
        gamma[c] = g;
        base_offset[c] = bo;
        alt_offset[c] = ao;
    }
    GainMapMeta {
        min_log2,
        max_log2,
        gamma,
        base_offset,
        alt_offset,
        base_headroom: p.base_hdr_headroom as f32,
        alt_headroom: p.alternate_hdr_headroom as f32,
        use_base_color_space: p.use_base_color_space,
    }
}

/// Oracle check (a): our bytes, parsed by ultrahdr-core's decoder.
#[test]
fn oracle_parses_our_bytes() {
    for meta in test_metas() {
        let bytes = iso21496::serialize(&meta);
        let parsed = parse_iso21496_fmt(&bytes, Iso21496Format::JxlJhgm)
            .expect("ultrahdr-core should parse our bytes");
        let parsed_meta = from_oracle(&parsed);
        assert_meta_close(&meta, &parsed_meta);
    }
}

/// Oracle check (b): ultrahdr-core's bytes, parsed by our decoder.
#[test]
fn we_parse_oracle_bytes() {
    for meta in test_metas() {
        let oracle_params = to_oracle(&meta);
        let bytes = serialize_iso21496_fmt(&oracle_params, Iso21496Format::JxlJhgm);
        let parsed = iso21496::parse(&bytes).expect("our parser should parse ultrahdr-core bytes");
        assert_meta_close(&meta, &parsed);
    }
}
