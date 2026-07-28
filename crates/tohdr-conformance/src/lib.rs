//! Independent conformance checker for `docs/acceptance-criteria.md`.
//!
//! `tohdr verify` reads our output with our own reader, so a defect present in
//! both the reader and the writer sails through it. This crate exists to make
//! that class of error visible, and its independence is a *build* property: the
//! manifest names no `tohdr-*` crate, so nothing here can call our parser even
//! by accident.
//!
//! What it does not write itself, it delegates to third parties rather than
//! reimplementing: `ultrahdr-core` parses the ISO 21496-1 payload and the Apple
//! MakerNote. What is left is the container walk (`isobmff`), the criteria, and
//! libavif's weight formula.

pub mod isobmff;

use isobmff::{Heif, Prop};
use ultrahdr_core::{AppleHdrInfo, GainMapMetadata, Iso21496Format};

pub const APPLE_URN: &str = "urn:com:apple:photo:2020:aux:hdrgainmap";

/// Criterion 4's payload size: one `ToneMapImage` version byte plus the 61-byte
/// single-channel C.2.2 struct — or 141 bytes when the struct carries three
/// channels, which the criteria doc lists as a legitimate improvement rather
/// than a defect. The size is fixed for a given channel count, so matching it is
/// the whole of "exact": a payload with trailing slack has the wrong length.
pub const ISO_PAYLOAD_LEN: usize = 62;
pub const ISO_PAYLOAD_LEN_3CH: usize = 142;

/// The length criterion 4 wants, from the payload's own channel-count flag: byte
/// 5 is the flags byte and bit 7 is `is_multichannel`. Reading one documented bit
/// is not a second parser — everything else comes from `ultrahdr-core`.
pub fn expected_payload_len(payload: &[u8]) -> usize {
    if payload.get(5).is_some_and(|f| f & 0x80 != 0) {
        ISO_PAYLOAD_LEN_3CH
    } else {
        ISO_PAYLOAD_LEN
    }
}

/// Displays the criteria are checked across, in stops.
const DISPLAYS: [f64; 6] = [1.0, 1.5, 2.0, 2.3, 2.98, 4.0];

/// Which signaling the file is expected to carry. Without one, the checker
/// reports what it finds; with one, a missing flavor is a failure rather than a
/// skip.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Flavor {
    Apple,
    Iso,
    Both,
}

impl Flavor {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "apple" => Ok(Flavor::Apple),
            "iso" | "ios" => Ok(Flavor::Iso),
            "both" => Ok(Flavor::Both),
            other => Err(format!("unknown flavor {other:?} (expected apple, iso, both)")),
        }
    }

    fn wants_apple(self) -> bool {
        self != Flavor::Iso
    }

    fn wants_iso(self) -> bool {
        self != Flavor::Apple
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    Pass,
    Fail,
    Skip,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Pass => "pass",
            Status::Fail => "fail",
            Status::Skip => "skip",
        }
    }
}

pub struct Check {
    pub criterion: u32,
    pub name: &'static str,
    pub status: Status,
    pub detail: String,
}

/// Everything the criteria read, gathered in one pass so a check is arithmetic
/// on facts rather than another walk of the file.
pub struct Info {
    pub size: usize,
    pub brands: Vec<String>,
    pub primary: Option<u32>,
    pub primary_type: Option<String>,
    pub primary_hidden: bool,
    pub base_size: Option<(u32, u32)>,
    pub gain: Option<u32>,
    pub gain_size: Option<(u32, u32)>,
    pub gain_bits: Option<Vec<u8>>,
    /// The item carrying the Apple gain-map URN, if any.
    pub apple_aux: Option<u32>,
    /// What that item's `auxl` reference points at.
    pub auxl_to: Vec<u32>,
    pub tmap: Option<u32>,
    pub tmap_dimg: Vec<u32>,
    pub tmap_payload_len: Option<usize>,
    /// The length criterion 4 wants, from the payload's own channel-count flag.
    pub tmap_payload_want: Option<usize>,
    pub iso: Option<GainMapMetadata>,
    pub iso_error: Option<String>,
    /// XMP `HDRGainMapHeadroom`, a linear multiplier rather than stops.
    pub xmp_headroom: Option<f64>,
    pub apple_hdr: Option<AppleHdrInfo>,
    /// `altr` entity groups, as entity id lists.
    pub altr: Vec<Vec<u32>>,
}

impl Info {
    pub fn has_apple(&self) -> bool {
        self.apple_aux.is_some()
    }

    pub fn has_iso(&self) -> bool {
        self.tmap.is_some()
    }

    /// The most gain the plane can deliver, over however many channels the
    /// payload declared. `ultrahdr-core` replicates a single channel into all
    /// three, so this is the same value either way.
    pub fn max_log2(&self) -> Option<f64> {
        let iso = self.iso.as_ref()?;
        Some(iso.channels.iter().map(|c| c.max).fold(f64::NEG_INFINITY, f64::max))
    }
}

/// libavif's gain-map weight (`src/gainmap.c:52-63`): linear in stops between
/// the two headrooms, clamped, negated when the alternate is the darker one.
pub fn gain_weight(base: f64, alt: f64, display: f64) -> f64 {
    if alt == base {
        return 0.0;
    }
    let w = ((display - base) / (alt - base)).clamp(0.0, 1.0);
    if alt < base { -w } else { w }
}

pub fn analyze(bytes: &[u8]) -> Result<Info, String> {
    let f = Heif::parse(bytes)?;

    let primary = f.primary;
    let primary_item = primary.and_then(|id| f.item(id));
    let base_size = primary.and_then(|id| ispe_of(&f, id));

    let apple_aux = f.items.iter().map(|i| i.id).find(|&id| {
        f.props_of(id).iter().any(|p| matches!(p, Prop::AuxC { urn } if urn == APPLE_URN))
    });
    let auxl_to = apple_aux
        .and_then(|id| f.refs_from(id, b"auxl"))
        .map(|r| r.to.clone())
        .unwrap_or_default();

    let tmap = f.items.iter().find(|i| i.typ == "tmap").map(|i| i.id);
    let tmap_dimg = tmap
        .and_then(|id| f.refs_from(id, b"dimg"))
        .map(|r| r.to.clone())
        .unwrap_or_default();

    // The gain map is the Apple auxiliary when there is one, and otherwise the
    // second `dimg` entry of the tone-mapped item -- an ISO-only file has no URN.
    let gain = apple_aux.or_else(|| tmap_dimg.get(1).copied());

    let (mut iso, mut iso_error, mut tmap_payload_len) = (None, None, None);
    let mut tmap_payload_want = None;
    if let Some(id) = tmap {
        match f.item_data(id) {
            Ok(payload) => {
                tmap_payload_len = Some(payload.len());
                tmap_payload_want = Some(expected_payload_len(&payload));
                match ultrahdr_core::parse_iso21496_fmt(&payload, Iso21496Format::AvifTmap) {
                    Ok(m) => iso = Some(m),
                    Err(e) => iso_error = Some(format!("{e:?}")),
                }
            }
            Err(e) => iso_error = Some(e),
        }
    }

    Ok(Info {
        size: bytes.len(),
        brands: f.brands.clone(),
        primary,
        primary_type: primary_item.map(|i| i.typ.clone()),
        primary_hidden: primary_item.is_some_and(|i| i.hidden),
        base_size,
        gain,
        gain_size: gain.and_then(|id| ispe_of(&f, id)),
        gain_bits: gain.and_then(|id| pixi_of(&f, id)),
        apple_aux,
        auxl_to,
        tmap,
        tmap_dimg,
        tmap_payload_len,
        tmap_payload_want,
        iso,
        iso_error,
        xmp_headroom: xmp_headroom(&f),
        apple_hdr: apple_hdr(&f),
        altr: f
            .groups
            .iter()
            .filter(|g| &g.typ == b"altr")
            .map(|g| g.entities.clone())
            .collect(),
    })
}

fn ispe_of(f: &Heif, id: u32) -> Option<(u32, u32)> {
    f.props_of(id).into_iter().find_map(|p| match p {
        Prop::Ispe { width, height } => Some((*width, *height)),
        _ => None,
    })
}

fn pixi_of(f: &Heif, id: u32) -> Option<Vec<u8>> {
    f.props_of(id).into_iter().find_map(|p| match p {
        Prop::Pixi { bits } => Some(bits.clone()),
        _ => None,
    })
}

/// The XMP copy of the headroom, from whichever RDF item carries it.
fn xmp_headroom(f: &Heif) -> Option<f64> {
    f.items
        .iter()
        .filter(|i| i.content_type == "application/rdf+xml")
        .filter_map(|i| f.item_data(i.id).ok())
        .find_map(|data| attr_f64(&String::from_utf8_lossy(&data), "HDRGainMapHeadroom"))
}

/// One XMP value, either spelling: `name="4.88"` as our writers emit it, or
/// `<ns:name>4.88</ns:name>` as the reference capture does. Not worth an XML
/// parser, but it is worth accepting both — reading only one made this checker
/// silently drop the copy on Apple's own files.
fn attr_f64(xmp: &str, name: &str) -> Option<f64> {
    let mut from = 0;
    while let Some(hit) = xmp.get(from..)?.find(name) {
        let rest = xmp.get(from + hit + name.len()..)?.trim_start();
        from += hit + name.len();
        let value = if let Some(r) = rest.strip_prefix('=') {
            let r = r.trim_start();
            let quote = r.chars().next()?;
            r.get(1..)?.split(quote).next()?
        } else if let Some(r) = rest.strip_prefix('>') {
            r.split('<').next()?
        } else {
            continue; // the xmlns declaration, or a longer tag name
        };
        if let Ok(v) = value.trim().parse() {
            return Some(v);
        }
    }
    None
}

/// Apple's MakerNote headroom, from Skia's `SkExif.cpp:83-95`.
///
/// `ultrahdr-core` has a `headroom_stops()` of its own, and it is wrong for
/// Apple's files: its `tag33 >= 1, tag48 > 0.01` branch is `-0.44·t + 2.86`
/// where Skia's is `-0.303·t + 2.303`, which puts the reference capture's
/// MakerApple copy 0.55 stops away from the ISO and XMP copies it is supposed to
/// agree with. So the tags come from ultrahdr-core and the arithmetic does not.
/// Four published coefficients are not an implementation worth sharing.
pub fn apple_headroom_stops(tag33: f64, tag48: f64) -> f64 {
    if tag33 < 1.0 {
        if tag48 <= 0.01 {
            -20.0 * tag48 + 1.8
        } else {
            -0.101 * tag48 + 1.601
        }
    } else if tag48 <= 0.01 {
        -70.0 * tag48 + 3.0
    } else {
        -0.303 * tag48 + 2.303
    }
}

/// Apple's HDR MakerNote tags, read by `ultrahdr-core` from the `Exif` item.
fn apple_hdr(f: &Heif) -> Option<AppleHdrInfo> {
    let exif = f.items.iter().find(|i| i.typ == "Exif")?;
    let data = f.item_data(exif.id).ok()?;
    // A HEIF `Exif` item begins with a 32-bit offset to the TIFF header. Trying
    // the whole payload too costs nothing and covers writers that omit it.
    let skip = 4 + u32::from_be_bytes(data.get(..4)?.try_into().ok()?) as usize;
    data.get(skip..)
        .and_then(ultrahdr_core::parse_exif_for_apple_hdr)
        .or_else(|| ultrahdr_core::parse_exif_for_apple_hdr(&data))
}

/// Criterion 8, split out so it is testable without a container around it.
///
/// `ultrahdr-core` reports tag 48 as `0.0` when absent, which is
/// indistinguishable from a written zero — so an absent tag 33 with a non-zero
/// tag 48 is the only way "48 without 33" can be detected here.
pub fn check_apple_tags(info: Option<&AppleHdrInfo>) -> (Status, String) {
    let Some(i) = info else {
        return (Status::Skip, "no Apple MakerNote HDR tags present".into());
    };
    let g = i.hdr_gain;
    match i.hdr_headroom {
        None if g == 0.0 => (Status::Skip, "no MakerApple headroom tags present".into()),
        None => (Status::Fail, format!("tag 48 = {g} with no tag 33 to select a branch")),
        Some(h) if g < 0.0 => (Status::Fail, format!("tag 48 = {g} is negative (tag 33 = {h})")),
        Some(h) if h < 0.0 => (Status::Fail, format!("tag 33 = {h} is negative")),
        Some(h) => (Status::Pass, format!("tag 33 = {h}, tag 48 = {g}")),
    }
}

pub fn check(info: &Info, expect: Option<Flavor>) -> Vec<Check> {
    let mut out = Vec::new();
    let mut add = |criterion, name, status, detail: String| {
        out.push(Check { criterion, name, status, detail });
    };

    if expect.is_none() {
        // Only meaningful without an expectation: with one, the flavor-specific
        // criteria below carry the same information and say which is missing.
        let ok = info.has_apple() || info.has_iso();
        add(
            0,
            "some gain-map signaling present",
            pass_if(ok),
            format!("apple={} iso={}", info.has_apple(), info.has_iso()),
        );
    }

    // 1. The base image is the primary item.
    match (info.primary, &info.primary_type) {
        (Some(id), Some(typ)) => {
            let ok = !info.primary_hidden
                && matches!(typ.as_str(), "hvc1" | "hev1" | "av01" | "grid" | "iden");
            add(
                1,
                "base image is the primary item",
                pass_if(ok),
                format!(
                    "pitm -> item {id} type {typ}{}",
                    if info.primary_hidden { ", hidden" } else { "" }
                ),
            );
        }
        (Some(id), None) => add(1, "base image is the primary item", Status::Fail,
            format!("pitm names item {id}, which is not in iinf")),
        (None, _) => add(1, "base image is the primary item", Status::Fail, "no pitm box".into()),
    }

    // 2. The gain map is single-channel 8-bit.
    match (info.gain, &info.gain_bits) {
        (Some(id), Some(bits)) => {
            let ok = bits.as_slice() == [8];
            let size = info.gain_size.map_or(String::new(), |(w, h)| format!("{w}x{h}, "));
            add(
                2,
                "gain map single-channel 8-bit",
                pass_if(ok),
                format!("item {id}: {size}{} channel(s) of {bits:?} bits", bits.len()),
            );
        }
        (Some(id), None) => add(2, "gain map single-channel 8-bit", Status::Fail,
            format!("item {id} has no pixi, so the channel count is unstated")),
        (None, _) => add(2, "gain map single-channel 8-bit", Status::Fail,
            "no gain-map item found".into()),
    }

    // A flavor's criteria are evaluated when it was asked for, or when the file
    // has it anyway. Otherwise they are still reported, as skips: a criterion
    // that vanishes from the output looks like one nobody thought about.
    let wanted = |has: bool, wants: fn(Flavor) -> bool| match expect {
        Some(f) if wants(f) => true,
        Some(_) => false,
        None => has,
    };

    // 3. Apple flavor: the URN and the back-reference to the base.
    if wanted(info.has_apple(), Flavor::wants_apple) {
        let base = info.primary;
        let (status, detail) = match (info.apple_aux, base) {
            (None, _) => (Status::Fail, format!("no item carries {APPLE_URN}")),
            (Some(id), Some(base)) if info.auxl_to.contains(&base) => {
                (Status::Pass, format!("item {id} has the URN and auxl -> {base}"))
            }
            (Some(id), Some(base)) => (
                Status::Fail,
                format!(
                    "item {id} has the URN but auxl {:?} does not reach pitm {base}",
                    info.auxl_to
                ),
            ),
            (Some(id), None) => {
                (Status::Fail, format!("item {id} has the URN but there is no pitm to reach"))
            }
        };
        add(3, "Apple URN + auxl to base", status, detail);
    } else {
        add(3, "Apple URN + auxl to base", Status::Skip, "no Apple signaling here".into());
    }

    // 4. ISO flavor: the tmap item, its dimg order, the brand, the payload size.
    if wanted(info.has_iso(), Flavor::wants_iso) {
        let want: Vec<u32> = [info.primary, info.gain].into_iter().flatten().collect();
        let brand = info.brands.iter().any(|b| b == "tmap");
        let (status, detail) = match info.tmap {
            None => (Status::Fail, "no tmap item".into()),
            Some(id) => {
                let dimg_ok = info.tmap_dimg == want && want.len() == 2;
                let len = info.tmap_payload_len;
                let len_ok = len.is_some() && len == info.tmap_payload_want;
                let detail = format!(
                    "item {id}: dimg {:?} (want {want:?}), tmap brand {brand}, payload {}",
                    info.tmap_dimg,
                    match (len, info.tmap_payload_want) {
                        (Some(n), Some(w)) => format!("{n} bytes (want {w})"),
                        _ => "unreadable".to_string(),
                    },
                );
                (pass_if(dimg_ok && brand && len_ok), detail)
            }
        };
        add(4, "tmap item, dimg [base,gain], tmap brand, exact payload", status, detail);
    } else {
        add(
            4,
            "tmap item, dimg [base,gain], tmap brand, exact payload",
            Status::Skip,
            "no ISO signaling here".into(),
        );
    }

    // 5, 6, 7, 10 all read the ISO payload.
    let iso = info.iso.as_ref();
    let unreadable = || {
        info.iso_error
            .clone()
            .unwrap_or_else(|| "no ISO 21496-1 payload in this file".into())
    };

    match (iso, info.max_log2()) {
        (Some(m), Some(max_log2)) => {
            let want = max_log2.max(0.0);
            let err = (m.alternate_hdr_headroom - want).abs();
            add(
                5,
                "max_log2 == alt_headroom",
                pass_if(err <= 1e-3),
                format!(
                    "max_log2 ={max_log2:.6} max(0,max_log2) ={want:.6} \
                     alt_headroom ={:.6} (err {err:.2e})",
                    m.alternate_hdr_headroom
                ),
            );
            add(
                6,
                "base_headroom == 0",
                pass_if(m.base_hdr_headroom.abs() <= 1e-9),
                format!("base_headroom = {:.6}", m.base_hdr_headroom),
            );
            add(7, "passes avifGainMapValidateMetadata", validate(m).0, validate(m).1);

            // 10. Every display gets every stop it can show.
            let mut worst = (f64::NAN, 0.0f64, 0.0f64, -1.0f64);
            for d in DISPLAYS {
                let w = gain_weight(m.base_hdr_headroom, m.alternate_hdr_headroom, d);
                let delivered = want * w;
                let expected = d.min(want);
                let err = (delivered - expected).abs();
                if err > worst.3 {
                    worst = (d, delivered, expected, err);
                }
            }
            add(
                10,
                "display receives every stop it can show",
                pass_if(worst.3 <= 1e-3),
                format!(
                    "worst at {:.2}-stop display: delivered {:.3}, \
                     expected min(display, max_log2)={:.3} (err {:.3})",
                    worst.0, worst.1, worst.2, worst.3
                ),
            );
        }
        _ => {
            for (n, name) in [
                (5u32, "max_log2 == alt_headroom"),
                (6, "base_headroom == 0"),
                (7, "passes avifGainMapValidateMetadata"),
                (10, "display receives every stop it can show"),
            ] {
                let status = if info.tmap.is_some() { Status::Fail } else { Status::Skip };
                add(n, name, status, unreadable());
            }
        }
    }

    // 8. MakerApple tag 48 non-negative.
    let (status, detail) = check_apple_tags(info.apple_hdr.as_ref());
    add(8, "MakerApple tag48 non-negative", status, detail);

    // 9. Every written copy of the headroom agrees. Compared as linear
    // multipliers, which is what XMP stores; the other two are stops.
    let mut copies: Vec<(&str, f64)> = Vec::new();
    if let Some(m) = iso {
        copies.push(("iso", m.alternate_hdr_headroom.exp2()));
    }
    if let Some(x) = info.xmp_headroom {
        copies.push(("xmp", x));
    }
    if let Some(i) = info.apple_hdr.as_ref()
        && let Some(t33) = i.hdr_headroom
    {
        copies.push(("makerapple", apple_headroom_stops(t33, i.hdr_gain).exp2()));
    }
    if copies.len() < 2 {
        add(
            9,
            "headroom copies agree",
            Status::Skip,
            format!("only {} copy present", copies.len()),
        );
    } else {
        let worst = copies
            .iter()
            .flat_map(|a| copies.iter().map(move |b| (a.1 - b.1).abs()))
            .fold(0.0f64, f64::max);
        let list: Vec<String> = copies.iter().map(|(k, v)| format!("{k}={v:.6}x")).collect();
        add(
            9,
            "headroom copies agree",
            pass_if(worst <= 1e-3),
            format!("{} worst delta={worst:.2e}", list.join(" ")),
        );
    }

    // 17. The tmap and the base are grouped `altr`. ImageIO reports no ISO gain
    // map at all without this, while every box in the file is otherwise correct.
    if info.tmap.is_some() {
        let want: Vec<u32> = [info.tmap, info.primary].into_iter().flatten().collect();
        let ok = info.altr.iter().any(|g| want.iter().all(|id| g.contains(id)));
        add(
            17,
            "tmap and base in one altr group",
            pass_if(ok && want.len() == 2),
            format!("altr groups {:?}, want both of {want:?}", info.altr),
        );
    } else {
        add(17, "tmap and base in one altr group", Status::Skip, "no tmap item".into());
    }

    // Reported in criterion order, not the order the facts happened to be read:
    // the numbers are how docs/acceptance-criteria.md is navigated.
    out.sort_by_key(|c| c.criterion);
    out
}

fn pass_if(ok: bool) -> Status {
    if ok { Status::Pass } else { Status::Fail }
}

/// Criterion 7. A zero denominator cannot reach here — `parse_iso21496_fmt`
/// rejects it outright — so what is left to check is libavif's own three rules
/// plus `ultrahdr-core`'s validator as a second opinion.
fn validate(m: &GainMapMetadata) -> (Status, String) {
    for (i, c) in m.channels.iter().enumerate() {
        let finite = [c.min, c.max, c.gamma, c.base_offset, c.alternate_offset]
            .iter()
            .all(|v| v.is_finite());
        if !finite {
            return (Status::Fail, format!("channel {i} has a non-finite value"));
        }
        if c.max < c.min {
            return (Status::Fail, format!("channel {i}: max {} < min {}", c.max, c.min));
        }
        if c.gamma <= 0.0 {
            return (Status::Fail, format!("channel {i}: gamma {} is not positive", c.gamma));
        }
    }
    if let Err(e) = ultrahdr_core::validate_gainmap_metadata(m) {
        return (Status::Fail, format!("ultrahdr-core rejects it: {e:?}"));
    }
    let c = &m.channels[0];
    (
        Status::Pass,
        format!("min={:.6} max={:.6} gamma={:.6}, denominators nonzero", c.min, c.max, c.gamma),
    )
}
