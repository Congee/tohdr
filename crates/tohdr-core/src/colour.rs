//! Which primaries the pixels are in, conversion between them, and reading the
//! answer out of an embedded ICC profile.
//!
//! Exists because the pipeline used to render everything into Rec.709 and clamp
//! what fell outside, which is where wide-gamut colour died -- 12.33% of a real
//! Lightroom P3 export, in a coherent yellow region rather than scattered
//! outliers. See docs/gamut.md.
//!
//! Rec.709's primaries sit strictly inside Display P3's, so 709 -> P3 is an exact
//! 3x3 matrix on linear light that cannot clip. The reverse is not, which is why
//! nothing here narrows on its own -- [`Primaries::narrower_than`] lets a caller
//! warn instead.
//!
//! All three sets are D65, so no chromatic adaptation is involved. It appears in
//! one place only: ICC profiles store colorants adapted to the D50 connection
//! space, so recognising one means undoing that first.

/// Which primaries a linear RGB buffer is expressed in.
///
/// Rec.2020 is here because Lightroom will hand us one — `Rec2020_hdr` is an
/// export colour space it offers, and it was the value sitting in LrC's
/// preferences on this machine — not because anything defaults to it.
/// No `Default`, deliberately. A wrong colour space is invisible in the output —
/// the pixels decode, they are merely the wrong colour — so there is no value
/// safe enough to inherit silently. Every construction site states one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Primaries {
    /// Rec.709 / sRGB primaries. The narrower set, and what every output was
    /// before this module existed.
    Bt709,
    /// Display P3 — same D65 white and sRGB transfer as sRGB, wider red and
    /// green. What every iPhone capture has carried since 2020, which is the
    /// compatibility argument for defaulting to it: Apple ships it by the
    /// billion, so a consumer that cannot read it is a consumer that cannot read
    /// photographs from any recent iPhone.
    DisplayP3,
    /// Rec.2020 primaries. Wider than P3, and wider than any display in this
    /// project's reach — accepted on input so a Rec.2020 export is not *misread*
    /// as something narrower.
    Bt2020,
}

impl Primaries {
    /// The ISO/IEC 23091-2 `colour_primaries` code for a `colr`/`nclx` box.
    pub fn nclx(self) -> u16 {
        match self {
            Primaries::Bt709 => 1,
            // 12 is SMPTE EG 432-1, the code Display P3 is signalled with; Apple
            // writes it on the `tmap` of the reference capture and exiftool renders it as
            // "SMPTE EG 432-1".
            Primaries::DisplayP3 => 12,
            Primaries::Bt2020 => 9,
        }
    }

    /// Name for logs and `--colour-space`.
    pub fn label(self) -> &'static str {
        match self {
            Primaries::Bt709 => "srgb",
            Primaries::DisplayP3 => "p3",
            Primaries::Bt2020 => "rec2020",
        }
    }

    /// Parse `--colour-space`, accepting the spellings a person is likely to type.
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.to_ascii_lowercase().replace(['-', '_', ' ', '.'], "").as_str() {
            "srgb" | "bt709" | "rec709" | "709" => Ok(Primaries::Bt709),
            "p3" | "displayp3" | "dcip3" => Ok(Primaries::DisplayP3),
            "rec2020" | "bt2020" | "2020" => Ok(Primaries::Bt2020),
            other => Err(format!(
                "unknown colour space {other:?} (expected srgb, p3, or rec2020)"
            )),
        }
    }

    /// Linear RGB → CIE XYZ, D65.
    pub const fn to_xyz(self) -> [[f64; 3]; 3] {
        match self {
            Primaries::Bt709 => RGB709_TO_XYZ,
            Primaries::DisplayP3 => P3_TO_XYZ,
            Primaries::Bt2020 => RGB2020_TO_XYZ,
        }
    }

    /// CIE XYZ → linear RGB, D65.
    pub const fn from_xyz(self) -> [[f64; 3]; 3] {
        match self {
            Primaries::Bt709 => XYZ_TO_RGB709,
            Primaries::DisplayP3 => XYZ_TO_P3,
            Primaries::Bt2020 => XYZ_TO_2020,
        }
    }

    /// Is `self` the narrower gamut, so that converting `other` into it clips?
    ///
    /// Ordered by area, and containment is strict for every pair that matters
    /// here — with one measured exception: **Display P3's red primary falls
    /// marginally outside Rec.2020's red–green edge**, so P3 → Rec.2020 is not
    /// quite lossless either. It is worth −6.05e-4 on a half-intensity P3 red
    /// (`p3_red_falls_marginally_outside_rec2020`), which is four orders of
    /// magnitude below the 21.6% excursions `probe_gamut.rs` measured going the
    /// other way, so treating 2020 as the wider of the two is right for every
    /// decision this answers.
    pub fn narrower_than(self, other: Primaries) -> bool {
        self.rank() < other.rank()
    }

    const fn rank(self) -> u8 {
        match self {
            Primaries::Bt709 => 0,
            Primaries::DisplayP3 => 1,
            Primaries::Bt2020 => 2,
        }
    }

    /// Every variant, for exhaustive matching in tests and ICC recognition.
    pub const ALL: [Primaries; 3] = [Primaries::Bt709, Primaries::DisplayP3, Primaries::Bt2020];
}

// --- linear RGB <-> XYZ, all D65 -------------------------------------------
//
// The first four are the constants `probe_gamut.rs` cross-checked its two renders
// against, agreeing with CoreGraphics to under 2e-4. Each pair is verified to be
// a mutual inverse by a test rather than by inspection.

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
const RGB2020_TO_XYZ: [[f64; 3]; 3] = [
    [0.6369580483, 0.1446169036, 0.1688809752],
    [0.2627002120, 0.6779980715, 0.0593017165],
    [0.0000000000, 0.0280726930, 1.0609850577],
];
const XYZ_TO_2020: [[f64; 3]; 3] = [
    [1.7166511880, -0.3556707838, -0.2533662814],
    [-0.6666843518, 1.6164812366, 0.0157685458],
    [0.0176398574, -0.0427706133, 0.9421031212],
];

/// Bradford XYZ D65 → D50, the adaptation ICC profiles are authored with.
///
/// Needed only to *recognise* a profile: its colorant tags describe the primaries
/// after adaptation into the D50 profile connection space, so a D65 reference
/// matrix does not match them until this is applied.
const BRADFORD_D65_TO_D50: [[f64; 3]; 3] = [
    [1.0478112, 0.0228866, -0.0501270],
    [0.0295424, 0.9904844, -0.0170491],
    [-0.0092345, 0.0150436, 0.7521316],
];

/// 3×3 product, `a · b`.
///
/// A `const fn` so every composed matrix below is *derived* from the primaries
/// rather than transcribed: a hand-copied product is a silent colour bug waiting
/// to happen, and this codebase has already had one worth 21 dB (see
/// `PlaneCodec::base_colour`).
pub const fn mat_mul(a: [[f64; 3]; 3], b: [[f64; 3]; 3]) -> [[f64; 3]; 3] {
    let mut out = [[0.0; 3]; 3];
    let mut i = 0;
    while i < 3 {
        let mut j = 0;
        while j < 3 {
            out[i][j] = a[i][0] * b[0][j] + a[i][1] * b[1][j] + a[i][2] * b[2][j];
            j += 1;
        }
        i += 1;
    }
    out
}

const IDENTITY: [[f64; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

/// The matrix taking linear `from` to linear `to`.
///
/// Exactly the identity when they match, rather than a product that rounds to it:
/// a no-op conversion must not perturb a single pixel.
pub const fn matrix(from: Primaries, to: Primaries) -> [[f64; 3]; 3] {
    if from.rank() == to.rank() {
        return IDENTITY;
    }
    mat_mul(to.from_xyz(), from.to_xyz())
}

/// Convert interleaved linear RGB in place.
///
/// `data` is RGB triples of linear light — [`crate::HdrRgb`]'s layout — and stays
/// extended-range: nothing is clamped, because clamping is the step that lost the
/// colour this module exists to keep. Converting into narrower primaries yields
/// negative components, and the caller decides what to do about them.
pub fn convert_linear_rgb(data: &mut [f32], from: Primaries, to: Primaries) {
    if from == to {
        return;
    }
    let m = matrix(from, to);
    let m = [
        [m[0][0] as f32, m[0][1] as f32, m[0][2] as f32],
        [m[1][0] as f32, m[1][1] as f32, m[1][2] as f32],
        [m[2][0] as f32, m[2][1] as f32, m[2][2] as f32],
    ];
    for px in data.chunks_exact_mut(3) {
        let (r, g, b) = (px[0], px[1], px[2]);
        px[0] = m[0][0] * r + m[0][1] * g + m[0][2] * b;
        px[1] = m[1][0] * r + m[1][1] * g + m[1][2] * b;
        px[2] = m[2][0] * r + m[2][1] * g + m[2][2] * b;
    }
}

// --- reading an embedded ICC profile ---------------------------------------

fn be_u32(b: &[u8], at: usize) -> Option<u32> {
    let s = b.get(at..at + 4)?;
    Some(u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}

/// `s15Fixed16Number` — ICC's signed 16.16 fixed point.
fn s15f16(b: &[u8], at: usize) -> Option<f64> {
    let s = b.get(at..at + 4)?;
    Some(i32::from_be_bytes([s[0], s[1], s[2], s[3]]) as f64 / 65536.0)
}

/// Find a tag's payload by its four-byte signature.
fn icc_tag<'a>(icc: &'a [u8], sig: &[u8; 4]) -> Option<&'a [u8]> {
    // 128-byte header, then a count, then 12 bytes per tag: signature, offset
    // from the start of the profile, size.
    let count = be_u32(icc, 128)? as usize;
    // A malformed count must not be trusted into an allocation or a long scan.
    if count > 1024 {
        return None;
    }
    for i in 0..count {
        let e = 132 + i * 12;
        let s = icc.get(e..e + 4)?;
        if s == sig {
            let off = be_u32(icc, e + 4)? as usize;
            let len = be_u32(icc, e + 8)? as usize;
            return icc.get(off..off.checked_add(len)?);
        }
    }
    None
}

/// The primaries an ICC profile describes, or `None` if it describes something
/// this crate has no matrix for.
///
/// Two routes, in order: a `cicp` tag stating the ISO/IEC 23091-2 code (ICC v4.4+,
/// which Apple writes), then the `rXYZ`/`gXYZ`/`bXYZ` colorants compared against
/// each known set after Bradford-adapting the reference to D50. The second is what
/// works on the 1998 sRGB profile Lightroom embeds, which predates `cicp`.
///
/// `None` rather than a guess: an Adobe RGB export read as sRGB is undetectably
/// wrong downstream -- just desaturated -- so the caller must be able to refuse.
pub fn primaries_from_icc(icc: &[u8]) -> Option<Primaries> {
    if let Some(cicp) = icc_tag(icc, b"cicp") {
        // 4-byte type signature, 4 reserved, then primaries/transfer/matrix/range.
        if let Some(&code) = cicp.get(8)
            && let Some(p) = Primaries::ALL.iter().find(|p| p.nclx() == code as u16)
        {
            return Some(*p);
        }
    }

    let mut got = [[0.0f64; 3]; 3];
    for (col, sig) in [b"rXYZ", b"gXYZ", b"bXYZ"].into_iter().enumerate() {
        let tag = icc_tag(icc, sig)?;
        // XYZType: 4-byte signature, 4 reserved, then one XYZNumber.
        for (row, out) in got.iter_mut().enumerate() {
            out[col] = s15f16(tag, 8 + row * 4)?;
        }
    }

    // 0.004 is loose against the ~1.5e-5 resolution of s15Fixed16 and tight
    // against the gap between candidates: sRGB and P3 red differ by 0.079 in X.
    // Nothing sits between them at this tolerance.
    let mut best: Option<(Primaries, f64)> = None;
    for p in Primaries::ALL {
        let want = mat_mul(BRADFORD_D65_TO_D50, p.to_xyz());
        let err = (0..3)
            .flat_map(|i| (0..3).map(move |j| (i, j)))
            .map(|(i, j)| (got[i][j] - want[i][j]).abs())
            .fold(0.0f64, f64::max);
        if best.is_none_or(|(_, b)| err < b) {
            best = Some((p, err));
        }
    }
    best.filter(|(_, err)| *err < 0.004).map(|(p, _)| p)
}

/// A profile's `desc` tag, for saying which profile could not be interpreted.
///
/// Handles both the v2 `textDescriptionType` (ASCII, length-prefixed) and the v4
/// `mluc` (UTF-16BE records). Best-effort: this only ever appears in a message.
pub fn icc_description(icc: &[u8]) -> Option<String> {
    let tag = icc_tag(icc, b"desc")?;
    match tag.get(0..4)? {
        b"desc" => {
            let len = be_u32(tag, 8)? as usize;
            let s = tag.get(12..12 + len.min(256))?;
            let s = s.split(|&b| b == 0).next().unwrap_or(s);
            Some(String::from_utf8_lossy(s).into_owned())
        }
        b"mluc" => {
            let len = be_u32(tag, 20)? as usize;
            let off = be_u32(tag, 24)? as usize;
            let s = tag.get(off..off + len.min(512))?;
            let units: Vec<u16> = s
                .chunks_exact(2)
                .map(|c| u16::from_be_bytes([c[0], c[1]]))
                .collect();
            Some(String::from_utf16_lossy(&units))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(m: [[f64; 3]; 3], v: [f64; 3]) -> [f64; 3] {
        [
            m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
            m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
            m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
        ]
    }

    fn close(m: [[f64; 3]; 3], want: [[f64; 3]; 3], tol: f64, what: &str) {
        for i in 0..3 {
            for j in 0..3 {
                assert!(
                    (m[i][j] - want[i][j]).abs() < tol,
                    "{what}: [{i}][{j}] = {} vs {}",
                    m[i][j],
                    want[i][j]
                );
            }
        }
    }

    #[test]
    fn nclx_codes_are_the_ones_apple_writes() {
        assert_eq!(Primaries::Bt709.nclx(), 1);
        assert_eq!(Primaries::DisplayP3.nclx(), 12);
        assert_eq!(Primaries::Bt2020.nclx(), 9);
    }

    #[test]
    fn parses_the_spellings_people_type() {
        for s in ["srgb", "sRGB", "bt709", "rec-709", "709"] {
            assert_eq!(Primaries::parse(s).unwrap(), Primaries::Bt709, "{s}");
        }
        for s in ["p3", "P3", "display-p3", "displayP3", "dci_p3"] {
            assert_eq!(Primaries::parse(s).unwrap(), Primaries::DisplayP3, "{s}");
        }
        for s in ["rec2020", "Rec.2020", "bt2020"] {
            assert_eq!(Primaries::parse(s).unwrap(), Primaries::Bt2020, "{s}");
        }
        assert!(Primaries::parse("prophoto").is_err());
    }

    /// Every transcribed pair must be a mutual inverse, which is the check that
    /// catches a mistyped digit in any of the six matrices above.
    #[test]
    fn each_primaries_set_inverts_itself() {
        for p in Primaries::ALL {
            close(
                mat_mul(p.from_xyz(), p.to_xyz()),
                IDENTITY,
                1e-6,
                &format!("{p:?} to_xyz/from_xyz"),
            );
        }
    }

    /// White stays white, or the conversion is a tint — the likeliest symptom of
    /// a transposed matrix and the cheapest to check.
    #[test]
    fn white_is_preserved_between_every_pair() {
        for from in Primaries::ALL {
            for to in Primaries::ALL {
                let out = apply(matrix(from, to), [1.0, 1.0, 1.0]);
                for (i, v) in out.iter().enumerate() {
                    assert!(
                        (v - 1.0).abs() < 1e-6,
                        "{from:?}->{to:?} channel {i} moved white to {v}"
                    );
                }
            }
        }
    }

    #[test]
    fn matched_spaces_give_exactly_the_identity() {
        for p in Primaries::ALL {
            assert_eq!(matrix(p, p), IDENTITY, "{p:?}");
        }
    }

    /// The property that makes P3 output free: every Rec.709 colour is inside P3,
    /// so the conversion never produces a negative component and never needs a
    /// clamp. Gamut corners are where it would show first.
    #[test]
    fn widening_never_goes_negative() {
        for (from, to) in [
            (Primaries::Bt709, Primaries::DisplayP3),
            (Primaries::Bt709, Primaries::Bt2020),
        ] {
            let m = matrix(from, to);
            for r in [0.0, 0.5, 1.0] {
                for g in [0.0, 0.5, 1.0] {
                    for b in [0.0, 0.5, 1.0] {
                        for (i, v) in apply(m, [r, g, b]).iter().enumerate() {
                            assert!(
                                *v >= -1e-9,
                                "{from:?}->{to:?} ({r},{g},{b}) channel {i} = {v}"
                            );
                        }
                    }
                }
            }
        }
    }

    /// Rec.2020 is *not* a strict superset of Display P3, which is easy to assume
    /// and wrong: P3's red primary sits just outside the Rec.2020 red–green edge,
    /// so even this "widening" produces a small negative.
    ///
    /// Pinned with a number because the size is the whole point — it is 6e-4,
    /// against the 21.6% excursions measured narrowing P3 to Rec.709. One is a
    /// rounding artefact of real gamut geometry; the other is visible colour.
    #[test]
    fn p3_red_falls_marginally_outside_rec2020() {
        let out = apply(matrix(Primaries::DisplayP3, Primaries::Bt2020), [0.5, 0.0, 0.0]);
        let worst = out.iter().cloned().fold(f64::INFINITY, f64::min);
        assert!(worst < 0.0, "expected a negative, got {out:?}");
        assert!(
            worst > -1e-3,
            "P3 red is barely outside Rec.2020; {worst} is too large to be that"
        );
    }

    /// And the converse, which is why nothing narrows on its own.
    #[test]
    fn narrowing_does_go_negative() {
        let out = apply(matrix(Primaries::DisplayP3, Primaries::Bt709), [0.0, 1.0, 0.0]);
        assert!(out.iter().any(|v| *v < -0.01), "P3 green fits in 709? {out:?}");
        assert!(Primaries::Bt709.narrower_than(Primaries::DisplayP3));
        assert!(Primaries::DisplayP3.narrower_than(Primaries::Bt2020));
        assert!(!Primaries::Bt2020.narrower_than(Primaries::Bt709));
        assert!(!Primaries::DisplayP3.narrower_than(Primaries::DisplayP3));
    }

    #[test]
    fn converting_a_buffer_matches_the_matrix_and_leaves_matched_spaces_alone() {
        let px = [0.25f32, 0.5, 0.75];
        let mut data = px.to_vec();
        convert_linear_rgb(&mut data, Primaries::Bt709, Primaries::Bt709);
        assert_eq!(data, px, "a no-op conversion rewrote the pixels");

        let mut data: Vec<f32> = px.to_vec();
        convert_linear_rgb(&mut data, Primaries::Bt709, Primaries::DisplayP3);
        let want = apply(
            matrix(Primaries::Bt709, Primaries::DisplayP3),
            [px[0] as f64, px[1] as f64, px[2] as f64],
        );
        for i in 0..3 {
            assert!((data[i] as f64 - want[i]).abs() < 1e-5, "channel {i}");
        }
    }

    /// Extended-range values are the point: an above-white highlight must convert
    /// like anything else rather than being clamped on the way through.
    #[test]
    fn above_white_survives_the_conversion() {
        let mut data = vec![4.0f32, 4.0, 4.0];
        convert_linear_rgb(&mut data, Primaries::Bt709, Primaries::DisplayP3);
        for v in &data {
            assert!((v - 4.0).abs() < 1e-4, "4x white became {v}");
        }
    }

    // --- ICC recognition, against numbers read off real files ---------------

    /// Build a minimal profile carrying just the three colorant tags.
    fn icc_with_colorants(m: [[f64; 3]; 3]) -> Vec<u8> {
        let mut icc = vec![0u8; 132];
        icc[128..132].copy_from_slice(&3u32.to_be_bytes());
        let mut body = Vec::new();
        for (col, sig) in [b"rXYZ", b"gXYZ", b"bXYZ"].into_iter().enumerate() {
            let off = 132 + 3 * 12 + body.len();
            icc.extend_from_slice(sig);
            icc.extend_from_slice(&(off as u32).to_be_bytes());
            icc.extend_from_slice(&20u32.to_be_bytes());
            body.extend_from_slice(b"XYZ ");
            body.extend_from_slice(&[0; 4]);
            for r in &m {
                body.extend_from_slice(&((r[col] * 65536.0).round() as i32).to_be_bytes());
            }
        }
        icc.extend_from_slice(&body);
        icc
    }

    /// The exact colorants exiftool reports for the profile Lightroom embeds in
    /// its HDR TIFF — the 1998 HP-authored `sRGB IEC61966-2.1`, which has no
    /// `cicp` tag, so this is the route that has to work.
    #[test]
    fn recognises_the_profile_lightroom_embeds() {
        let lr_srgb = [
            [0.43607, 0.38515, 0.14307],
            [0.22249, 0.71687, 0.06061],
            [0.01392, 0.09708, 0.71410],
        ];
        assert_eq!(
            primaries_from_icc(&icc_with_colorants(lr_srgb)),
            Some(Primaries::Bt709)
        );
    }

    /// Derived rather than transcribed: whatever Bradford-adapting each reference
    /// gives must round-trip back to that reference.
    #[test]
    fn recognises_every_known_set_from_its_colorants() {
        for p in Primaries::ALL {
            let d50 = mat_mul(BRADFORD_D65_TO_D50, p.to_xyz());
            assert_eq!(primaries_from_icc(&icc_with_colorants(d50)), Some(p), "{p:?}");
        }
    }

    /// A profile this crate has no matrix for must come back `None`, not the
    /// nearest guess. Adobe RGB (1998) is the realistic case — an export space
    /// Lightroom offers — and reading it as sRGB would silently desaturate every
    /// pixel with nothing downstream able to notice.
    #[test]
    fn refuses_a_profile_it_has_no_matrix_for() {
        // Adobe RGB (1998) colorants, D50-adapted, as its ICC profile stores them.
        let adobe_rgb = [
            [0.60974, 0.20528, 0.14919],
            [0.31111, 0.62567, 0.06322],
            [0.01947, 0.06087, 0.74457],
        ];
        assert_eq!(primaries_from_icc(&icc_with_colorants(adobe_rgb)), None);
    }

    /// `cicp` wins when present, because it states the code rather than implying
    /// it. Apple writes this tag.
    #[test]
    fn a_cicp_tag_is_read_directly() {
        let mut icc = vec![0u8; 132];
        icc[128..132].copy_from_slice(&1u32.to_be_bytes());
        icc.extend_from_slice(b"cicp");
        icc.extend_from_slice(&144u32.to_be_bytes());
        icc.extend_from_slice(&12u32.to_be_bytes());
        icc.resize(144, 0);
        icc.extend_from_slice(b"cicp");
        icc.extend_from_slice(&[0; 4]);
        icc.extend_from_slice(&[12, 13, 1, 1]); // P3 primaries, sRGB transfer
        assert_eq!(primaries_from_icc(&icc), Some(Primaries::DisplayP3));
    }

    /// Truncated and malformed profiles are input from outside, so they must
    /// return `None` rather than panic on a slice.
    #[test]
    fn malformed_profiles_do_not_panic() {
        assert_eq!(primaries_from_icc(&[]), None);
        assert_eq!(primaries_from_icc(&[0u8; 131]), None);
        let mut absurd = vec![0u8; 132];
        absurd[128..132].copy_from_slice(&u32::MAX.to_be_bytes());
        assert_eq!(primaries_from_icc(&absurd), None);
        // A tag table pointing past the end of the profile.
        let mut past = vec![0u8; 132];
        past[128..132].copy_from_slice(&1u32.to_be_bytes());
        past.extend_from_slice(b"rXYZ");
        past.extend_from_slice(&9999u32.to_be_bytes());
        past.extend_from_slice(&20u32.to_be_bytes());
        assert_eq!(primaries_from_icc(&past), None);
        assert_eq!(icc_description(&[0u8; 8]), None);
    }

    #[test]
    fn reads_a_v2_description() {
        let mut icc = vec![0u8; 132];
        icc[128..132].copy_from_slice(&1u32.to_be_bytes());
        let text = b"Adobe RGB (1998)\0";
        icc.extend_from_slice(b"desc");
        icc.extend_from_slice(&144u32.to_be_bytes());
        // The declared size must cover the whole payload, or `icc_tag` is right to
        // refuse it: signature + reserved + count + string.
        icc.extend_from_slice(&(12 + text.len() as u32).to_be_bytes());
        icc.resize(144, 0);
        icc.extend_from_slice(b"desc");
        icc.extend_from_slice(&[0; 4]);
        icc.extend_from_slice(&(text.len() as u32).to_be_bytes());
        icc.extend_from_slice(text);
        assert_eq!(icc_description(&icc).as_deref(), Some("Adobe RGB (1998)"));
    }
}
