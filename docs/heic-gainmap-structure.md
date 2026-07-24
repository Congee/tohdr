# HEIC Gain-Map Container Structure — Reverse-Engineering Notes

Target files (all read-only, never modified — see verification at the bottom):

| Label | Path | Notes |
|---|---|---|
| **IMG_4913** (gold reference) | `~/Downloads/IMG_4913.HEIC` | iPhone-native HDR photo, 5712x4284, 8-bit base. Renders HDR correctly everywhere. |
| **DSC07752** | `~/Desktop/DSC07752.heic` | Third-party export, 9504x6336, 8-bit. Washed out in iOS WeChat. |
| **DSC07752_iso** | `~/Desktop/DSC07752_iso.heic` | Same source, re-encoded with an "ISO gain map" flag, 10-bit. Has `tmap` brand but still washed out. |

Method: a from-scratch ~250-line stdlib-only Python ISOBMFF/HEIF box parser (no third-party libraries), cross-checked against `exiftool -v3` / `-a -u -G1` output. Scripts live in `~/.claude/jobs/d8bbb591/tmp/` (throwaway, not committed to this repo). Every table below is generated from that parser's output on the actual files; byte offsets are absolute file offsets unless noted.

---

## 1. `ftyp` comparison

| | IMG_4913 (gold) | DSC07752 | DSC07752_iso |
|---|---|---|---|
| major_brand | `heic` | `heic` | `heix` |
| minor_version | 0 | 0 | 0 |
| compatible_brands (in order) | `mif1 MiHB MiHA heix MiHE MiPr miaf heic tmap` | `mif1 MiHE miaf MiHB heic` | `mif1 MiHB MiHA heix miaf heic tmap` |
| has `tmap` brand | **yes** | no | yes |
| has `MiHA`/`MiPr` (Apple HDR-adjunct brands) | **yes (both)** | no | `MiHA` only, no `MiPr` |

Note the gold file advertises **both** `tmap` (ISO/IEC 23008-12 gain-map amendment) and Apple's private `MiHA`/`MiPr`/`MiHE`/`MiHB` brands in the same `compatible_brands` list — it is deliberately dual-format. DSC07752 advertises neither `tmap` nor `MiHA`/`MiPr`. DSC07752_iso added `tmap` and `MiHA` but not `MiPr`, and also swapped major_brand from `heic`→`heix` and dropped `MiHE`/`MiPr`.

---

## 2. Item graph comparison

### IMG_4913 (gold) — full item graph

`meta` box tree (order as written): `hdlr, dinf(dref), pitm, iinf, iref, iprp(ipco,ipma), grpl(altr), idat, iloc`.

- `pitm`: primary_item_id = **46**
- `iinf`: 123 items total. Relevant non-tile items:

| item_id | type | hidden | role |
|---|---|---|---|
| 1–45 | hvc1 | yes | base-image HEVC tiles (45 tiles) |
| **46** | grid | no | **primary/base image** (5712x4284, assembled from 1–45) |
| 47–61 | hvc1 | yes | gain-map HEVC tiles (15 tiles) |
| **62** | grid | yes | **Apple aux HDR gain map** (2856x2142, assembled from 47–61) |
| 63 | hvc1 | yes | linear-thumbnail-adjacent image (referenced by `cdsc` from 64) |
| 64 | mime | yes | metadata for item 63 |
| 65 | hvc1 | no | thumbnail (`thmb`→46) |
| 66–113 | hvc1 | yes | tiles for grid 114 (48 tiles) |
| 114 | grid | yes | linear thumbnail / style-delta grid (4096x3072, aux of 46 & 122) |
| 115, 117, 119 | hvc1 | yes | further aux images (portrait matte / skin matte / sky matte family), each `auxl`→[46,122] |
| 116, 118 | mime | yes | metadata for 115 / 117 |
| 120 | mime | yes | metadata, `cdsc`→62 (metadata **for the gain map**) |
| **122** | tmap | no | **ISO 21496-1 tone-map item** |
| 123 | `uri ` | yes | item_name="metadata", `cdsc`→[46,122] |
| 124 | Exif | yes | Exif block, `cdsc`→[46,122] |

`iref` entries (type, from → to):

```
dimg   46  -> [1..45]                (base grid tiles)
dimg   62  -> [47..61]                (gain-map grid tiles)
auxl   62  -> [46]                    (item 62 is an AUX image OF item 46)
auxl   63  -> [46, 122]
cdsc   64  -> [63]
thmb   65  -> [46]
dimg  114  -> [66..113]
auxl  114  -> [46, 122]
auxl  115  -> [46, 122]
cdsc  116  -> [115]
auxl  117  -> [46, 122]
cdsc  118  -> [117]
auxl  119  -> [46, 122]
cdsc  120  -> [62]
dimg  122  -> [46, 62]                (tmap references BASE then GAIN-MAP, in that order)
cdsc  123  -> [46, 122]
cdsc  124  -> [46, 122]
```

**Key structural fact**: IMG_4913 has *both* the Apple aux-image gain map (item 62, `auxl`→46, `auxC` = `urn:com:apple:photo:2020:aux:hdrgainmap`) **and** the ISO `tmap` item (122). The `tmap` item does not carry pixel data of its own — it is a *derived* item whose `dimg` reference list is `[46, 62]`: base image first, gain map second. This is the graph a decoder walks to do ISO-standard reconstruction, while decoders that only understand Apple's own `auxC` convention can find the same gain map (62) directly off the primary item without ever looking at 122.

### DSC07752 — item graph (relevant items)

- `pitm`: 81
- item 81 = grid (base, 9504x6336, 8-bit, not hidden)
- item 162 = grid (hidden), `dimg`→[82..161] (80 tiles), `auxl`→[81]. `auxC` = **`urn:mpeg:hevc:2015:auxid:1`** (the *generic* HEVC auxiliary-picture URN, not Apple's gain-map URN). ispe attached = 9504x6336 (full base resolution), pixi = 1 channel, 8-bit.
- item 243 = grid (hidden), `dimg`→[163..242] (80 tiles), `auxl`→[81]. `auxC` = **`urn:com:apple:photo:2020:aux:hdrgainmap`**. Same ispe (9504x6336, full res), pixi = 1 channel, 8-bit.
- item 244 = mime, `cdsc`→243 (metadata for 243)
- item 245 = Exif, `cdsc`→81
- item 246 = mime, `cdsc`→81
- **No `tmap` item exists in this file at all.**

So DSC07752 has *two* aux images at full base resolution: one correctly URN-tagged as Apple's gain map (243) and a second, oddly-URN-tagged aux image (162) using the generic HEVC aux-picture URN. There is no ISO tone-map item, matching the missing `tmap` brand.

### DSC07752_iso — item graph (relevant items)

- `pitm`: 81
- item 81 = grid (base, 9504x6336, colr=`prof`/Rec.709, pixi 3ch×10-bit)
- item 162 = grid (hidden), `dimg`→[82..161], ispe=9504x6336 (full res, **not** downscaled), pixi **3 channels × 8-bit** (RGB gain map, not grayscale). **No `auxC` property at all** — item 162 is *not* declared as an auxiliary image by any mechanism other than being a `tmap` dimg target. It also carries an essential `colr` `nclx` property: primaries=9 (BT.2020), transfer=16 (SMPTE ST 2084 / PQ), matrix=6 or 9 depending on entry — i.e. the gain-map picture itself is tagged with an HDR/PQ colour space, which is semantically odd for what should be a linear/grayscale delta map.
- item 163 = **tmap**, not hidden, `dimg`→[81, 162] (base then gain-map, same order convention as the gold file)
- item 165 = Exif, `cdsc`→[81, 163]
- item 166 = mime, `cdsc`→[81, 163]

So DSC07752_iso *does* build the same `dimg`=[base, gainmap] shape as the gold file's `tmap` item, but:
1. the gain-map item has no `auxC` property and no `auxl` reference back to the base — Apple-convention–only decoders (and even some ISO decoders that discover aux images before checking `tmap`) will not find it at all;
2. the gain map is full resolution and 3-channel RGB (not the gold file's half-resolution, 1-channel, 8-bit grayscale);
3. the gain-map's own colour tagging (BT.2020/PQ) doesn't match its role as a difference/ratio map.

---

## 3. Property list (`ipco`) and `ipma` — essentials

### IMG_4913 `ipco` (29 entries, order as stored)

| idx | type | decoded |
|---|---|---|
| 1 | colr | `prof`, ICC 536B, desc="Display P3" |
| 2 | ispe | 640x896 (thumbnail-family) |
| 3 | ispe | 5712x4284 (**base**) |
| 4 | irot | 0° |
| 5 | pixi | 3ch, 8/8/8 |
| 6 | ispe | 2856x2142 (**gain map**, exactly half base res) |
| 7 | pixi | 1ch, 8-bit (**gain map**, grayscale) |
| 8 | auxC | `urn:com:apple:photo:2020:aux:hdrgainmap` |
| 9 | colr | `prof`, ICC 572B, desc="sRGB IEC61966-2.1 Linear" |
| 10 | ispe | 2016x1512 |
| 11 | auxC | `urn:com:apple:photo:2020:aux:semanticskymatte` |
| 12 | ispe | 416x312 |
| 13 | colr | `prof`, ICC 560B, desc="Display P3 Linear" |
| 14 | ispe | 512x512 |
| 15 | ispe | 4096x3072 |
| 16 | pixi | 3ch, 10/10/10 |
| 17 | auxC | `tag:apple.com,2023:photo:aux:styledeltamap` |
| 18 | auxC | `urn:com:apple:photo:2018:aux:portraiteffectsmatte` |
| 19 | auxC | `urn:com:apple:photo:2019:aux:semanticskinmatte` |
| 20 | ispe | 1024x768 |
| 21 | auxC | `tag:apple.com,2023:photo:aux:linearthumbnail` |
| 22 | colr | `prof`, ICC **26664B**, desc="Display P3 Primaries; PQ (Adaptive Gain Curve 81B7427DF220A6FA)" — **attached to the `tmap` item (122), essential** |
| 23 | colr | `nclx`, primaries=2(unspecified), transfer=2(unspecified), matrix=2(unspecified), full_range=1 — attached to gain map (62), essential |
| 24–29 | hvcC | present, HEVC decoder configs (see §6); contents beyond profile/level not fully decoded |

Essential `ipma` associations that matter:

- item 46 (base): `colr`(1, essential) + `ispe`(3) + `pixi`(5) + `irot`(4, essential)
- item 62 (gain map): `ispe`(6) + `pixi`(7) + `auxC`(8, **essential**) + `colr`(23, **essential**, plain nclx) + `irot`(4, essential)
- item 122 (`tmap`): `colr`(22, **essential**, the 26KB ICC "Adaptive Gain Curve" profile) + `ispe`(3, non-essential, base's own 5712x4284) + `pixi`(16, non-essential, 10/10/10) + `irot`(4, essential)

So the *reconstructed HDR output* is characterized by a large **ICC profile** attached to the `tmap` item (not a plain `nclx`), while the gain map picture itself is tagged with a throwaway "unspecified" `nclx`. The big ICC profile's description string literally says "PQ (Adaptive Gain Curve ...)" — Apple encodes the tone-mapping curve information partly through this profile, not solely through the ISO metadata payload.

### DSC07752 `ipco` (10 entries)

| idx | type | decoded |
|---|---|---|
| 1 | colr | `prof`, ICC 556B, desc="Rec. ITU-R BT.709-5" |
| 2 | ispe | 1024x832 |
| 3 | ispe | 9504x6336 (base, and reused for **both** aux images — not downscaled) |
| 4 | irot | 0° |
| 5 | pixi | 3ch 8/8/8 (base) |
| 6 | pixi | 1ch 8-bit (both aux images) |
| 7 | auxC | `urn:mpeg:hevc:2015:auxid:1` — attached to item 162, **essential** |
| 8 | auxC | `urn:com:apple:photo:2020:aux:hdrgainmap` — attached to item 243, **essential** |
| 9, 10 | hvcC | present, not fully decoded |

No `clli`, no PQ/HDR `nclx`, no ICC profile beyond a plain Rec.709 SDR one.

### DSC07752_iso `ipco` (10 entries)

| idx | type | decoded |
|---|---|---|
| 1 | colr | `prof`, ICC 556B, desc="Rec. ITU-R BT.709-5" (base) |
| 2 | ispe | 1024x832 |
| 3 | ispe | 9504x6336 (base **and** gain map, non-essential on both) |
| 4 | irot | 0° |
| 5 | pixi | 3ch 10/10/10 (base) |
| 6 | colr | `nclx` primaries=2,transfer=2,matrix=6,full_range=1 — attached to gain map (162), essential |
| 7 | pixi | 3ch 8/8/8 (gain map — **RGB, not grayscale**) |
| 8 | hvcC | present, not decoded |
| 9 | colr | `nclx` primaries=9(BT.2020),transfer=16(PQ),matrix=9(BT.2020 ncl),full_range=1 — attached to `tmap` item 163, **essential** |
| 10 | hvcC | present, not decoded |

The `tmap` item's "output characterization" here is a small `nclx` (BT.2020/PQ) rather than a full ICC profile — structurally valid per spec, but a completely different mechanism than the gold file's ICC-based approach, and (per §5) numerically inconsistent with the gain map's own encoded range.

No `clli` box was found in **any** of the three files.

---

## 4. The gain map, specifically

| | IMG_4913 (gold) | DSC07752 | DSC07752_iso |
|---|---|---|---|
| item id | 62 | 243 (+ a bogus extra, 162) | 162 |
| `auxC` URN | `urn:com:apple:photo:2020:aux:hdrgainmap` | `urn:com:apple:photo:2020:aux:hdrgainmap` (243); `urn:mpeg:hevc:2015:auxid:1` (162, wrong) | **none** |
| dimensions | 2856x2142 = **exactly 1/2** base (5712x4284) | 9504x6336 = **1:1** base (no downscale) | 9504x6336 = **1:1** base (no downscale) |
| channels / bit depth | **1 channel, 8-bit** (grayscale) | 1 channel, 8-bit (both 162 and 243) | **3 channels, 8-bit** (RGB) |
| colour tag | plain `nclx` (all "unspecified") | none decoded beyond base pixi | `nclx` BT.2020/PQ/BT.2020ncl (semantically wrong for a gain map) |
| linked via `auxl` to base | yes | yes (both items) | **no** |
| linked via `tmap` `dimg` | yes (item 122) | n/a (no tmap) | yes (item 163) |

---

## 5. The `tmap` item and its ISO 21496-1 payload

### Where the payload lives

The `tmap` item's data is **not** a property — it's the item's own bytes, accessed through `iloc` with `construction_method = 1` (idat-relative), pointing into the `idat` box.

- IMG_4913: `idat` box body at file offset **33764**, size 94. Item 122's `iloc` extent = offset 24, length 62 (relative to idat body) → absolute file bytes **[33788, 33850)**.
- DSC07752_iso: `idat` box body at file offset **6012**, size 158. Item 163's `iloc` extent = offset 16, length 142 → absolute file bytes **[6028, 6170)**.

Extracted verbatim (byte-for-byte, nothing else) to:
- `~/dev/tohdr/assets/fixtures/img4913_iso21496.bin` (62 bytes)
- `~/dev/tohdr/assets/fixtures/dsc07752_iso21496.bin` (142 bytes)

### Decoding method

There is no public ISO/IEC 21496-1 text available in this environment (WebFetch disabled). Instead I pulled the open-source reference **encoder/decoder** from `google/libultrahdr` (`lib/include/ultrahdr/gainmapmetadata.h` + `lib/src/gainmapmetadata.cpp`, fetched via `curl` from `raw.githubusercontent.com`, Apache-2.0/MIT licensed, an implementation of this exact ISO format), and validated the byte layout against the raw bytes by brute-forcing the header length until every rational field's denominator came out self-consistent. Header length 6 bytes made **all 7 (IMG_4913) / all 14 (DSC07752_iso) rational denominators identical and sane** (1,000,000 and 100,000 respectively) — strong confirmation the layout below is correct, further cross-validated against exiftool's independently-parsed XMP `HDRGainMapHeadroom` tag (see below — matches to 4+ significant figures).

Struct (single-channel case, `is_multichannel` bit clear):

```
u8  minimum_version        (0 = version 0)
u8  writer_version
u8  reserved               x3  (all zero in both samples; exact bit meaning per spec not confirmed)
u8  flags                  bit7=is_multichannel, bit6=use_base_colour_space,
                            bit3=use_common_denominator (google-private encoding
                            optimization, unset in both real-world samples below),
                            bit2=backward_direction
--- if multichannel bit clear (1 "channel" worth of fields): ---
s32 base_hdr_headroom_num   / u32 base_hdr_headroom_den
s32 alternate_hdr_headroom_num / u32 alternate_hdr_headroom_den
s32 gain_map_min_num        / u32 gain_map_min_den
s32 gain_map_max_num        / u32 gain_map_max_den
u32 gain_map_gamma_num      / u32 gain_map_gamma_den
s32 base_offset_num         / u32 base_offset_den
s32 alternate_offset_num    / u32 alternate_offset_den
--- if multichannel bit set: the last 5 pairs repeat x3 (R,G,B), still after
    the two shared headroom pairs ---
```

All numeric fields are `numerator / denominator`; `min_content_boost = 2^(gain_map_min)`, `max_content_boost = 2^(gain_map_max)`, `hdr_capacity_min = 2^(base_hdr_headroom)`, `hdr_capacity_max = 2^(alternate_hdr_headroom)`.

### IMG_4913 payload — field-by-field (62 bytes)

```
offset  bytes                  field                         value
------  ---------------------  ----------------------------  ----------------------------
0-1     00 00                  minimum_version                0
2-3     00 00                  writer_version                  0
4       00                     reserved
5       40                     flags                           0x40 = use_base_colour_space=1,
                                                                 is_multichannel=0, backward=0
6-9     00 00 00 00            base_hdr_headroom_num            0
10-13   00 0f 42 40            base_hdr_headroom_den            1,000,000
                                -> hdr_capacity_min = 2^0.0    = 1.0
14-17   00 22 e6 05            alternate_hdr_headroom_num       2,287,109
18-21   00 0f 42 40            alternate_hdr_headroom_den       1,000,000
                                -> hdr_capacity_max = 2^2.287109 = 4.880771
22-25   ff ff f8 55            gain_map_min_num (signed)        -1,963
26-29   00 0f 42 40            gain_map_min_den                 1,000,000
                                -> min_content_boost = 2^-0.001963 = 0.99864
30-33   00 22 e6 05            gain_map_max_num                 2,287,109
34-37   00 0f 42 40            gain_map_max_den                 1,000,000
                                -> max_content_boost = 2^2.287109 = 4.880771
38-41   00 0c 99 54            gain_map_gamma_num                825,684
42-45   00 0f 42 40            gain_map_gamma_den                1,000,000
                                -> gamma = 0.825684
46-49   00 00 00 0a            base_offset_num (signed)          10
50-53   00 0f 42 40            base_offset_den                   1,000,000
                                -> offset_sdr = 0.00001
54-57   00 00 00 0a            alternate_offset_num (signed)     10
58-61   00 0f 42 40            alternate_offset_den              1,000,000
                                -> offset_hdr  = 0.00001
```

Full hexdump (62 bytes):

```
00000000: 0000 0000 0040 0000 0000 000f 4240 0022  .....@......B@."
00000010: e605 000f 4240 ffff f855 000f 4240 0022  ....B@...U..B@."
00000020: e605 000f 4240 000c 9954 000f 4240 0000  ....B@...T..B@..
00000030: 000a 000f 4240 0000 000a 000f 4240       ....B@......B@
```

**Cross-validation**: `exiftool -a -u -G1` reports `[XMP-HDRGainMap] HDR Gain Map Headroom : 4.880772` for IMG_4913, against `2^2.287109 = 4.880771` from these bytes — agreement to 1e-6, so the field mapping is confirmed independently of the libultrahdr-derived struct.

Third independent agreement: decoding the MakerApple tags of the same file through Skia's formula (`tag33=1.00999999`, `tag48=0.05253907666`, see `crates/tohdr-core/src/apple.rs`) yields **4.880675x**, within 1e-4 of both. Apple writes this headroom three times — ISO `tmap` payload, XMP, and MakerNote — and all three agree.

**The extra header byte, resolved by arithmetic**: this 6-byte header is one byte longer than the 5 that libavif writes (`minimum_version` u16 + `writer_version` u16 + flags). The offset-4 "reserved" byte above is the wrong reading — the byte is a leading `ToneMapImage` version at offset **0** (ISO/IEC 23008-12 6.6.2.4.2), which wraps the C.2.2 struct inside a `tmap` item, with the two versions shifted to bytes 1-2 and 3-4. The file sizes prove it: libavif's C.2.2 struct is 61 bytes for one channel and 141 for three, and the two real payloads are exactly **62 = 1 + 61** and **142 = 1 + 141**. A 5-byte header cannot produce 142 at all (`142 - 5 - 16 = 121`, not a multiple of the 40-byte per-channel block).

Both readings decode identical field values, since either way the fractions begin at byte 6 — but only the version-prefix reading generalizes. `crates/tohdr-core/tests/iso21496.rs` asserts this against both fixtures: strip one byte, and our parser reads Apple's real payload field-for-field.

### DSC07752_iso payload — decode (142 bytes, multichannel)

```
header: min_version=0, writer_version=0, reserved=0,0,0, flags=0xC0
        (is_multichannel=1, use_base_colour_space=1, backward=0)

base_hdr_headroom      =      0 / 100000 = 0.0        -> hdr_capacity_min = 1.0
alternate_hdr_headroom = 356847 / 100000 = 3.56847     -> hdr_capacity_max = 2^3.56847 = 11.8636

channel R: min=0/100000=0.0 max=196000/100000=1.96 (boost=3.8906) gamma=1.0 off_sdr=0.00001 off_hdr=0.00001
channel G: identical to R
channel B: identical to R
```

142 bytes consumed exactly (0 leftover) — clean decode.

**Cross-validation**: `exiftool` reports `[XMP-HDRGainMap] HDR Gain Map Headroom : 11.863581` for **DSC07752.heic** (the non-ISO sibling from the same source) — matches `2^3.56847 = 11.8636` almost exactly. That number is carried through from the original camera/tool metadata into both re-encodes.

**But it does not match the gain map's own encoded range**: the metadata claims `hdr_capacity_max = 11.86x`, but the gain-map picture itself only encodes up to `max_content_boost = 3.89x` (`gain_map_max = 1.96` in log2). In the gold file these two numbers are **identical** (both 2.287109 → 4.880771x) by construction. This 3x discrepancy in DSC07752_iso is a real, decodable, structural defect — not a guess.

Why that produces a washed-out render specifically on a phone: the standard weight is `w = clamp((display_hr - base_hr) / (alt_hr - base_hr), 0, 1)`, all in log2 stops (libavif `src/gainmap.c:61`). With `base_hr = 0` and `alt_hr = 3.568`, a ~2.3-stop phone display gets `w = 0.645`, so only 1.26 of the map's 1.96 encoded stops are applied. A Mac XDR panel (~2.98 stops) gets `w = 0.835` → 1.64 stops, noticeably closer to full. The gold file's `alt_hr = 2.287` is *below* both displays' headroom, so `w` clamps to 1.0 and the map applies in full everywhere. That asymmetry matches the reported symptom — fine on the Mac, washed on the phone — but it is a prediction from the metadata, not a measurement of any particular app's renderer.

---

## 6. Apple MakerNote / XMP HDR tags

| tag | IMG_4913 | DSC07752 | DSC07752_iso |
|---|---|---|---|
| MakerNote tag 33 `HDRHeadroom` | 1.00999999 | 1 | 1 |
| MakerNote tag 48 `HDRGain` | 0.05253907666 | -0.008120966145 | -0.008120966145 |
| XMP `HDRGainMapVersion` | 0.2.0.0 | 0.2.0.0 | **absent** |
| XMP `HDRGainMapHeadroom` | 4.880772 | 11.863581 | **absent** |
| QuickTime `AuxiliaryImageType` (exiftool's own detection) | `urn:com:apple:photo:2020:aux:hdrgainmap` | `urn:com:apple:photo:2020:aux:hdrgainmap` | **absent** |

Note DSC07752 and DSC07752_iso report **byte-identical** MakerNote `HDRHeadroom`/`HDRGain` values (1 / -0.008120966145) despite being different encodes of a Sony-sourced photo — these look like boilerplate/pass-through values injected by the re-encoding tool rather than per-image computed values (Apple's own maker-note HDR fields are normally scene-specific, as seen varying in IMG_4913). DSC07752_iso additionally lost the XMP `HDRGainMap*` block and the `AuxiliaryImageType` detection entirely — consistent with §2/§4's finding that its gain map item carries no `auxC` property.

---

## 7. ICC profiles

| file | # colr `prof`/`rICC` entries | descriptions | attached to |
|---|---|---|---|
| IMG_4913 | 4 | "Display P3" (536B) / "sRGB IEC61966-2.1 Linear" (572B) / "Display P3 Linear" (560B) / "Display P3 Primaries; PQ (Adaptive Gain Curve 81B7427DF220A6FA)" (**26,664B**) | base(46) / a linear-thumbnail-family item / another linear-family item / **tmap(122), essential** |
| DSC07752 | 1 | "Rec. ITU-R BT.709-5" (556B) | base(81) |
| DSC07752_iso | 1 | "Rec. ITU-R BT.709-5" (556B) | base(81) — the `tmap` item here uses a plain `nclx` instead (see §3) |

The gold file's largest ICC profile (26,664 bytes, extracted verbatim to `assets/fixtures/img4913_tmap_icc_profile.icc`) is attached to the `tmap` item as an **essential** property. Its `desc` tag literally names an "Adaptive Gain Curve" with a per-image hex ID (`81B7427DF220A6FA`) — this is presumably Apple's private mechanism for embedding the actual tone-mapping curve as an ICC transform, layered on top of (and possibly redundant with, or complementary to) the ISO 21496-1 numeric metadata in §5. **I could not decode the internal ICC tag table beyond the `desc` string** — the profile almost certainly contains a `para`/curve or LUT tag encoding the adaptive gain curve itself, but I did not reverse-engineer ICC tag internals beyond extracting `desc`. Flagging as *present, not decoded*.

---

## 8. Annotated box tree — IMG_4913.HEIC (the target shape for a writer)

```
ftyp                                              @0       size=52
  major_brand='heic', compatible_brands=[mif1,MiHB,MiHA,heix,MiHE,MiPr,miaf,heic,tmap]
meta                                               @52      size=35782
  hdlr                                             @64      size=33
  dinf                                             @97      size=36
    dref                                           @105     size=28
  pitm                                              @133     size=14      primary_item_id=46
  iinf                                              @147     size=2726    123 items (infe v2 entries)
  iref                                              @2873    size=476     dimg/auxl/thmb/cdsc graph (§2)
  iprp                                              @3349    size=30371
    ipco                                            @3357    size=29641   29 properties (§3)
    ipma                                            @32998   size=722     item->property associations
  grpl                                              @33720   size=36
    altr                                            @33728   size=28      alternative-item group
  idat                                              @33756   size=94      inline item data (grid headers + tmap payload)
  iloc                                              @33850   size=1984    item location table (123 entries)
mdat                                                @35834   size=2420970 all coded HEVC tile bytestreams
```

A writer targeting this exact shape must, in order: `ftyp` → `meta` (with children in the order `hdlr, dinf, pitm, iinf, iref, iprp, grpl, idat, iloc`) → `mdat`. Within `iprp`, `ipco` must precede `ipma`. `idat` must precede `iloc` in this file (though ISOBMFF doesn't strictly require this ordering — it's what ImageIO does). Item data for grid/tmap items lives in `idat` via `construction_method=1`; HEVC tile data lives in `mdat` via `construction_method=0`.

---

## 9. What the broken files are missing (concrete, actionable)

**DSC07752.heic** (no `tmap`, washed out in WeChat):
1. **No ISO `tmap` item at all**, and no `tmap` in `compatible_brands`. Any decoder that requires ISO 21496-1 discovery (rather than Apple's private `auxC` URN) finds nothing to tone-map with. *Fix: emit a `tmap` item with `dimg`→[base, gainmap] and the ISO 21496-1 metadata payload in `idat`, and add `tmap` to `ftyp` compatible_brands.*
2. Gain map is full base resolution (9504x6336) instead of downscaled — not a correctness bug per se, but a big deviation from the reference encoder's convention (half-res, item 62 in the gold file) and wastes space/decode time.
3. A second aux item (162) exists with the wrong URN (`urn:mpeg:hevc:2015:auxid:1`, the generic HEVC-aux URN) at the same resolution as the real gain map (243) — likely leftover cruft from the encoding pipeline that could confuse a decoder scanning aux images by URN prefix match instead of exact string.

**DSC07752_iso.heic** (has `tmap`, still washed out):
1. **The gain-map item (162) has no `auxC` property at all** — it is only reachable through the `tmap` item's `dimg` list, so any decoder that discovers gain maps via the Apple aux-image convention (as many real-world decoders do, in addition to or instead of full ISO 21496-1 support) will not find it. *Fix: attach `auxC` = `urn:com:apple:photo:2020:aux:hdrgainmap` to the gain-map item in addition to the `tmap` reference, exactly as the gold file does for item 62.*
2. **No `auxl` reference from the gain map back to the base image** — only the `tmap`'s `dimg` link exists. The gold file has both.
3. **The ISO metadata's `alternate_hdr_headroom` (11.86x) does not match the gain map's own encoded `gain_map_max` (3.89x)**. A spec-compliant decoder that trusts the headroom field to decide how much to boost, but is clamped by the actual gain-map pixel range, will under- or over-shoot — this is a real numeric inconsistency in the payload itself, not just a missing-link problem. In the gold file these two values are identical by construction.
4. Gain map is **3-channel RGB** at full base resolution instead of the gold file's **1-channel grayscale at half resolution** — larger, and tagged with an HDR/PQ `nclx` color property (BT.2020/PQ) on what should be a plain difference map. Semantically inconsistent property tagging on the gain-map item itself.
5. XMP `HDRGainMap*` tags and the MakerNote `AuxiliaryImageType` detection are absent entirely (§6) — consistent with #1: exiftool's own heuristics for "this file has a gain map" fail on this file the same way a real decoder's heuristics likely do.

---

## 10. What I could not decode

- **HEVC `hvcC` box internals** beyond `configurationVersion`, `general_profile_space`, `general_tier_flag`, `general_profile_idc`, and `general_level_idc` (these five fields have fixed, unambiguous byte offsets per ISO/IEC 14496-15 and were spot-checked: e.g. IMG_4913 tile hvcC entries show `profile_idc=3` (Main Still Picture) and `profile_idc=2` (Main 10) at `level_idc` 63 and 90 respectively). Chroma format, exact bit depth fields, parallelism type, and the embedded NAL unit arrays (VPS/SPS/PPS) were **not** decoded — present, contents not decoded.
- **ICC tag-table internals** beyond the `desc` (ProfileDescription) tag — in particular the presumed curve/LUT tag inside IMG_4913's 26,664-byte "Adaptive Gain Curve" profile that likely encodes the actual per-image tone curve. Extracted verbatim as a fixture for further analysis; not reverse-engineered here.
- **The exact bit-level meaning of the 3 "reserved" bytes** in the ISO 21496-1 header (offsets 2-4, always zero in both samples observed) — inferred to be reserved/padding by elimination (no other byte-count fits both samples' data cleanly), not confirmed against spec text (unavailable in this environment).
- **The `useCommonDenominator` flag bit (0x08)** — this is documented in `google/libultrahdr`'s own source as a private encoding optimization on top of the ISO format; neither real-world sample here sets it, so its exact on-wire semantics (which the code does implement) were not needed to decode these two files, but a writer should know the alternate "common denominator" wire form exists.
- **`altr` group semantics** in IMG_4913's `grpl` box (present, contents not decoded — likely an "alternative representations" grouping for cross-image-format switching, but the specific member list/logic was not parsed out).

---

## Fixtures extracted (`assets/fixtures/`)

| file | contents |
|---|---|
| `img4913_iso21496.bin` | Raw 62-byte ISO 21496-1 GainMapMetadata payload from IMG_4913's `tmap` item (122), byte-exact from `idat`. |
| `img4913_auxc_urn.txt` | All 6 distinct `auxC` URN/tag strings found in IMG_4913, one per line. |
| `img4913_tmap_icc_profile.icc` | The 26,664-byte ICC profile attached to IMG_4913's `tmap` item (desc: "Display P3 Primaries; PQ (Adaptive Gain Curve 81B7427DF220A6FA)"). |
| `dsc07752_iso21496.bin` | Raw 142-byte ISO 21496-1 GainMapMetadata payload from DSC07752_iso's `tmap` item (163), byte-exact from `idat`. |

---

## Input file integrity

```
before:
733b37ddbb88dcca73804f944472502413dc03fb22696a4e380cf6773618f4cf  IMG_4913.HEIC
00eb103fcce448c746f2cffeb37ec80f869d5f9b36fc904befb7e7d32cde7988  DSC07752.heic
7c895f9df259048087703f2f7f758e77fb062f3dd9636f8508d5f951c297d41a  DSC07752_iso.heic

after (identical):
733b37ddbb88dcca73804f944472502413dc03fb22696a4e380cf6773618f4cf  IMG_4913.HEIC
00eb103fcce448c746f2cffeb37ec80f869d5f9b36fc904befb7e7d32cde7988  DSC07752.heic
7c895f9df259048087703f2f7f758e77fb062f3dd9636f8508d5f951c297d41a  DSC07752_iso.heic
```

All three inputs were opened strictly read-only throughout this investigation.
