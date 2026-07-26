//! Encoder-facing contracts: which gain-map flavor to emit, and how to hit a
//! byte budget.
//!
//! Both engines implement [`GainMapEncoder`] over the same options so their
//! outputs are directly comparable — same base, same plane, same metadata, only
//! the container writer differs.

use crate::{GainMapMeta, GainPlane, Rgb};

/// Which gain-map signaling to write.
///
/// These are not interchangeable in the wild: a consumer that only knows
/// Apple's auxiliary-image convention ignores a `tmap`, and an ISO 21496-1
/// consumer ignores the Apple URN. `IMG_4913.HEIC` — the reference file that
/// renders correctly everywhere tested — carries **both**, which is why
/// [`Flavor::Both`] is the default.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Flavor {
    /// `urn:com:apple:photo:2020:aux:hdrgainmap` auxiliary image, an `auxl`
    /// reference back to the base, and the MakerApple headroom tags. What
    /// Apple's own capture pipeline has written since 2020.
    Apple,
    /// ISO 21496-1: a `tmap` derived item whose `dimg` lists the base and the
    /// gain map, carrying the C.2.2 metadata payload, plus the `tmap` brand in
    /// `ftyp`. The interoperable, vendor-neutral form.
    Iso,
    /// Both of the above over one shared gain-map image item, matching
    /// `IMG_4913.HEIC`. Costs a few hundred bytes of extra boxes and no extra
    /// pixel data.
    #[default]
    Both,
}

impl Flavor {
    pub fn writes_apple(self) -> bool {
        matches!(self, Flavor::Apple | Flavor::Both)
    }

    pub fn writes_iso(self) -> bool {
        matches!(self, Flavor::Iso | Flavor::Both)
    }
}

/// Knobs an engine honors when muxing.
///
/// Borrows `exif` rather than owning it so the type stays `Copy`:
/// [`encode_within_budget`] derives a fresh `EncodeOptions` per quality it tries,
/// and a block that is 20 KB on an iPhone capture should not be cloned once per
/// binary-search step.
#[derive(Clone, Copy, Debug)]
pub struct EncodeOptions<'a> {
    pub flavor: Flavor,
    /// Base-image quality, `1..=100`. Engine-specific in meaning; both engines
    /// map it onto their codec's rate control.
    pub base_quality: u8,
    /// Gain-plane quality, `1..=100`. Can run lower than the base: the plane is
    /// a smooth, low-frequency correction, and Apple ships it at half
    /// resolution for the same reason.
    pub gain_quality: u8,
    /// The source's Exif block to carry through, as a standalone TIFF structure
    /// with no `exif_tiff_header_offset` prefix — what
    /// `tohdr_portable::exif::read` returns. `None` writes no Exif, which is
    /// what every caller did before this existed.
    ///
    /// An engine that cannot carry it must say so rather than drop it quietly;
    /// see [`GainMapEncoder::metadata_support`].
    pub exif: Option<&'a [u8]>,
    /// The source's own XMP packet: keywords, caption, rating, IPTC, rights —
    /// everything a photographer typed rather than the camera recorded.
    ///
    /// An engine must *merge* rather than replace, since it also has a headroom
    /// packet of its own to state; [`crate::xmp::merge_headroom_into`] is that
    /// merge. `None` means the source had none, and the engine writes only its
    /// own packet.
    pub xmp: Option<&'a [u8]>,
    /// The source's IPTC-IIM block, for a backend that needs it apart from the
    /// Exif block it also lives in. See
    /// [`crate::exif::wrap_in_jpeg_with_iptc`].
    pub iptc: Option<&'a [u8]>,
    /// Non-image metadata items to copy through untouched, e.g. Apple's
    /// Photographic Styles plist. Empty is the norm for non-HEIF sources.
    pub opaque_items: &'a [crate::OpaqueItem],
    /// How the stored pixels have to be transformed for display, from the
    /// source's Exif `Orientation`.
    ///
    /// No engine here rotates pixels, so a rotated source stays correct only if
    /// the container says so — and it has to say the same thing the Exif tag in
    /// the same file says. See [`crate::orient`].
    pub orientation: crate::HeifTransform,
}

impl Default for EncodeOptions<'_> {
    fn default() -> Self {
        Self {
            flavor: Flavor::default(),
            base_quality: 85,
            gain_quality: 85,
            exif: None,
            iptc: None,
            xmp: None,
            opaque_items: &[],
            orientation: crate::HeifTransform::default(),
        }
    }
}

/// A backend that can mux a base image plus a gain map into one container.
///
/// Engine A (Apple ImageIO) and Engine B (portable, hpvca + our own muxer) both
/// implement this so their outputs can be diffed against each other and against
/// an iPhone reference file.
pub trait GainMapEncoder {
    type Error: core::fmt::Debug;

    /// Label for logs and benchmark tables, e.g. `"apple-imageio"`.
    fn name(&self) -> &'static str;

    /// Which of [`EncodeOptions`]' metadata fields this backend actually writes.
    ///
    /// Defaults to [`crate::MetadataSupport::NONE`] so a backend has to claim
    /// each capability rather than inherit it. The point is that a caller can
    /// tell the user what was dropped, instead of a field being silently ignored
    /// by an engine that never looked at it.
    fn metadata_support(&self) -> crate::MetadataSupport {
        crate::MetadataSupport::NONE
    }

    fn encode(
        &self,
        base: &Rgb,
        gain: &GainPlane,
        meta: &GainMapMeta,
        opts: &EncodeOptions,
    ) -> Result<Vec<u8>, Self::Error>;
}

/// Outcome of a budgeted encode.
#[derive(Debug)]
pub struct Budgeted {
    pub bytes: Vec<u8>,
    /// The `base_quality` that produced `bytes`.
    pub quality: u8,
    /// How many encodes the search ran.
    pub attempts: u32,
    /// Whether `bytes.len() <= max_bytes`. False means even `min_quality`
    /// overshot, and `bytes` is the smallest output found — the caller decides
    /// whether to downscale or fail.
    pub within_budget: bool,
}

/// Encode the largest-quality output that still fits in `max_bytes`.
///
/// Binary-searches `base_quality` over `min_quality..=opts.base_quality`,
/// carrying `gain_quality` along proportionally so the plane does not stay
/// expensive while the base is starved.
///
/// # Monotonicity assumption
///
/// Binary search is only correct if output size is non-decreasing in quality.
/// For both engines' rate control that holds in practice but is not guaranteed
/// by any codec: a quantizer step can occasionally cost bytes elsewhere. The
/// consequence of a local inversion is a slightly suboptimal quality pick, never
/// an over-budget result, because every returned candidate is one whose measured
/// size was checked directly — the search never extrapolates.
pub fn encode_within_budget<E: GainMapEncoder>(
    engine: &E,
    base: &Rgb,
    gain: &GainPlane,
    meta: &GainMapMeta,
    opts: &EncodeOptions,
    max_bytes: u64,
    min_quality: u8,
) -> Result<Budgeted, E::Error> {
    let hi_q = opts.base_quality.max(1);
    let lo_q = min_quality.clamp(1, hi_q);
    let gain_ratio = opts.gain_quality as f32 / hi_q as f32;

    let mut attempts = 0;
    let mut run = |q: u8| -> Result<Vec<u8>, E::Error> {
        attempts += 1;
        let o = EncodeOptions {
            base_quality: q,
            gain_quality: ((q as f32 * gain_ratio).round() as u8).clamp(1, 100),
            ..*opts
        };
        engine.encode(base, gain, meta, &o)
    };

    // Full quality first: the common case is that it already fits, and that
    // costs one encode instead of log2(range).
    let full = run(hi_q)?;
    if full.len() as u64 <= max_bytes {
        return Ok(Budgeted {
            bytes: full,
            quality: hi_q,
            attempts,
            within_budget: true,
        });
    }

    // Nothing fits below the floor either -> report the floor's output as the
    // smallest we can do, rather than silently shipping something oversized at
    // a higher quality.
    let floor = run(lo_q)?;
    if floor.len() as u64 > max_bytes {
        return Ok(Budgeted {
            bytes: floor,
            quality: lo_q,
            attempts,
            within_budget: false,
        });
    }

    // Invariant: `best` always fits, `hi` never does.
    let mut best = (lo_q, floor);
    let mut lo = lo_q;
    let mut hi = hi_q;
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        let out = run(mid)?;
        if out.len() as u64 <= max_bytes {
            best = (mid, out);
            lo = mid;
        } else {
            hi = mid;
        }
    }

    Ok(Budgeted {
        bytes: best.1,
        quality: best.0,
        attempts,
        within_budget: true,
    })
}

/// Parse a human byte size: `4MB`, `4 MiB`, `3.5m`, `1500000`.
///
/// `MB`/`M` are decimal (10^6) and `MiB` binary (2^20), per IEC — a user who
/// types `--max-size 4MB` for an email attachment limit means 4,000,000.
pub fn parse_size(s: &str) -> Result<u64, String> {
    let t = s.trim();
    if t.is_empty() {
        return Err("empty size".into());
    }
    let split = t
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(t.len());
    let (num, unit) = t.split_at(split);
    let value: f64 = num
        .parse()
        .map_err(|_| format!("not a number: {num:?}"))?;
    if value < 0.0 || !value.is_finite() {
        return Err(format!("size must be finite and non-negative: {s:?}"));
    }
    let mult: f64 = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1.0,
        "k" | "kb" => 1e3,
        "kib" => 1024.0,
        "m" | "mb" => 1e6,
        "mib" => 1024.0 * 1024.0,
        "g" | "gb" => 1e9,
        "gib" => 1024.0 * 1024.0 * 1024.0,
        other => return Err(format!("unknown size unit: {other:?}")),
    };
    let bytes = value * mult;
    if bytes > u64::MAX as f64 {
        return Err(format!("size too large: {s:?}"));
    }
    Ok(bytes as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_parse() {
        assert_eq!(parse_size("4MB").unwrap(), 4_000_000);
        assert_eq!(parse_size("4MiB").unwrap(), 4 * 1024 * 1024);
        assert_eq!(parse_size("4 mib").unwrap(), 4 * 1024 * 1024);
        assert_eq!(parse_size("3.5m").unwrap(), 3_500_000);
        assert_eq!(parse_size("1500000").unwrap(), 1_500_000);
        assert_eq!(parse_size("512k").unwrap(), 512_000);
        assert!(parse_size("").is_err());
        assert!(parse_size("banana").is_err());
        assert!(parse_size("4PB").is_err());
    }

    #[test]
    fn flavor_coverage() {
        assert!(Flavor::Both.writes_apple() && Flavor::Both.writes_iso());
        assert!(Flavor::Apple.writes_apple() && !Flavor::Apple.writes_iso());
        assert!(!Flavor::Iso.writes_apple() && Flavor::Iso.writes_iso());
        assert_eq!(Flavor::default(), Flavor::Both);
    }
}
