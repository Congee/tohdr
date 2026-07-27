# Metadata Passthrough — what a conversion keeps, and what it cannot

A conversion used to produce a file with **no Exif at all**: no camera, no lens,
no exposure, no date, no location. That gap is closed, and so is most of what was
left after it. This document records what is carried, what is not, and — for each
thing that is not — whether that is a decision, a platform limit, or work
outstanding.

Everything here is measured on real files: an iPhone 17 Pro HDR reference capture
(5712×4284), a 16-bit TIFF with Lightroom-shaped metadata injected, and a JPEG
derived from it. Tag counts are `exiftool -a -G1 -s` group counts.

## 1. What a conversion keeps

Same scene through both engines, against the source:

| group | reference | Engine B | Engine A |
|---|---|---|---|
| `IFD0` | 9 | **9** | **9** |
| `ExifIFD` | 32 | **32** | **32** |
| `GPS` | 15 | **15** | **15** |
| `Apple` (MakerNote) | 25 | **25** | **25** |
| `PLIST` (Photographic Styles) | 110 | **110** | 0 |
| `Composite` (derived) | 20 | **20** | **20** |
| `XMP-HDRGainMap` | 2 | **2** | **2** |
| `XMP-apdi` + matte versions | 7 | 0 | 0 |
| ICC profiles | 4 (113 tags) | 0 | 0 |

Cost in bytes, Engine B: 2,114,582 → 2,175,798. Of that, `idat` is 61,502 bytes,
almost all of it the Photographic Styles plist; the Exif block is 4 KB of it.

For a source without Apple's private data — the Lightroom path — what survives is
the part a photographer typed:

| | source | Engine B | Engine A |
|---|---|---|---|
| `Orientation` | Rotate 90 CW | ✅ | ✅ |
| `dc:subject` (keywords) | sunset, golden gate | ✅ | ✅ |
| `xmp:Rating` | 4 | ✅ | ✅ |
| `dc:title`, `dc:description` | ✅ | ✅ | ✅ |
| `Artist`, `Copyright` | ✅ | ✅ | ✅ |
| IPTC `By-line`, `Keywords` | ✅ | ✅ | ❌ (§5) |
| GPS | ✅ | ✅ | ✅ |

All three source containers are supported, and the CLI reports which one it
found (`exif_source` in `--json`):

| source | where Exif lives | where XMP lives | where IPTC lives | reported as |
|---|---|---|---|---|
| HEIF/HEIC | the `Exif` item, past `exif_tiff_header_offset` | a `mime` item, `application/rdf+xml` | `IFD0` tag `33723` | `heif-exif-item` |
| JPEG | the `APP1` tagged `Exif\0\0` | the `APP1` tagged with Adobe's namespace | **`APP13`**, not the Exif IFD | `jpeg-app1` |
| TIFF | the file's own `IFD0` | `IFD0` tag `700` | `IFD0` tag `33723` | `tiff-ifd0` |

## 2. The Exif block is rebuilt, and what that costs

`tohdr_portable::exif` re-serializes rather than copies, emitting in the
*source's* byte order so no value is ever byte-swapped. A RATIONAL is two LONGs
rather than one 8-byte quantity, which is exactly what a byte-order conversion
gets wrong, so the tests cover a big-endian source specifically.

Selection is a **denylist** (`IFD0_DROP`), so a tag nobody here has heard of is
carried rather than lost — `ImageUniqueID`, IPTC, a private tag, all survive. The
list removes three classes and nothing else:

- **Offsets into bytes the output does not contain.** `StripOffsets`, `SubIFDs`,
  `TileOffsets`, `DNGPrivateData`. A reader that knows these tags *will* follow
  them, and only we know they no longer lead anywhere. This is the one class
  where the failure is worse than a missing tag.
- **How the source's pixels were laid out.** `BitsPerSample`, `Compression`,
  `PhotometricInterpretation`, `SampleFormat` and the rest. The output's pixels
  are 8-bit YCbCr in HEVC whatever arrived, so copying these would state
  something false rather than lose something true.
- **Two tags carried better elsewhere**: the XMP packet (`700`) gets its own
  item, and an embedded ICC (`34675`) is the `colr` box's business. Neither is
  lost from the output; both would be a second, competing authority inside Exif.

### `MakerNote` is pinned, not relocated

Apple's, Canon's and Sony's maker notes address their own contents with offsets
relative to the enclosing TIFF header, so a relocated one no longer parses.
Instead of detecting vendors, [`serialize`] puts the value back at the **exact
block-relative offset the source had it at**, padding to reach it. The bytes then
see the same addresses they were written for, whichever vendor wrote them.
The reference capture needs 12 bytes of padding; all 25 Apple tags read back with
byte-identical values.

Pinning can fail only for a source laid out backwards — values before IFDs — in
which case the tag is dropped and `dropped_maker_note` says so. A conventional
source cannot reach that state, because this module only ever *removes* entries,
so its IFD region cannot grow past where the source's values began.

### One vendor value is rewritten: MakerApple tag 48

The only place a conversion edits a vendor's bytes rather than copying them, and
it is not optional. `acceptance-criteria.md` §9 requires every copy of the
headroom in one file to agree within `1e-3`, because a consumer picks one and a
file whose copies disagree is one where somebody reads the wrong number. The
source's tag 48 describes the *source's* headroom; this conversion derives its
own. On the reference capture they differ by 0.019 stops — a 1.3% over-declaration
riding into a file whose ISO payload says otherwise, which is the exact defect
criterion 5 exists to catch.

So `align_apple_headroom` rewrites tag 48 in place, keeping its denominator and
its offset so the note's length never changes and the pin stays valid. Measured:

```
tag48  0.05253907666 -> 0.1150496461
iso=4.817026x  xmp=4.817019x  maker=4.817017x   worst delta 9.16e-06
```

Only tag 48: `headroom_from_tags` uses tag 33 solely to pick a branch at the
`1.0` threshold and never numerically, so a source value already above `1.0`
decodes the same either way and stays as the camera wrote it. Above 3.0 stops no
tag 48 can express the headroom without understating it, and then both tags are
removed in place — §8's "silence is correct", with the other 23 tags kept.

`tools/verify_gainmap.py` criterion 9 now includes the MakerApple copy. It did
not before, which is how a stale copy could have ridden along unnoticed; the
extended check passes on Apple's own file (worst delta 9.59e-05) and would fail
on a verbatim carry.

## 3. XMP is merged, not replaced

The source's packet is the photographer's — keywords, title, caption, rating,
IPTC, rights, develop history — and this pipeline has a headroom packet of its
own to state. `merge_headroom_into` inserts one `rdf:Description` before the
closing `</rdf:RDF>` and leaves every other byte where the source put it. An
`rdf:RDF` element may hold any number of descriptions, so the result is
well-formed by construction rather than by luck, and no partial XMP model gets a
chance to drop the schemas it does not know.

Which XMP is carried is decided by the container, not by a heuristic. HEIF's
`cdsc` reference means *this item describes that one*, and the reference capture uses it
to draw exactly the needed line:

```
cdsc: 64->[63]  116->[115]  118->[117]  120->[62]  123->[46, 122]  124->[46, 122]
```

Items 46 and 122 are the primary image and its `tmap`. So the Exif item (124) and
the Photographic Styles plist (123) describe the photograph, while four XMP items
describe auxiliary images — a sky matte, a skin matte, a portrait-effects matte,
and the gain map. **Those four are dropped correctly**: carrying them would state
that this file contains mattes it does not. That is the whole of the `XMP-apdi`
row in §1.

## 4. Orientation reaches the container

Nothing here rotates pixels, so a rotated source can only stay correct if the
container says how to display it — and that has to agree with the Exif tag the
same file carries. `tohdr_core::orient` maps each Exif `Orientation` onto an
`irot`/`imir` pair, and both are written, so an Exif reader and a HEIF reader
reach the same answer.

The algebra is checked against Exif's own coordinate definition for all eight
values at every pixel of a non-square image. What that could not settle was which
way round `imir`'s `axis` field reads, and the first answer was wrong:

```
exif in   engine            irot  imir   read back   verdict
      2   portable-hpvca       0     0           4   MISMATCH
      4   portable-hpvca       0     1           2   MISMATCH
      5   portable-hpvca       1     1           7   MISMATCH
      7   portable-hpvca       1     0           5   MISMATCH
```

The spec's "a vertical (axis = 0) or horizontal (axis = 1) axis for the mirroring
operation" reads as *about* a vertical axis, i.e. left and right swap. It is the
opposite: the field names the direction the image is flipped in, so `axis = 0`
swaps top and bottom. Four of eight orientations were wrong in a way no amount of
re-reading the sentence would have shown. With the axes swapped, all 16 files (8
orientations × 2 engines) read back as the orientation their source stated —
`examples/probe_orientation.rs`, pinned by `tests/orientation_roundtrip.rs`.

Engine A agreed with the source throughout, because it hands ImageIO the
orientation number and lets ImageIO write the boxes. That is what makes it a
usable oracle: the disagreement could only be in our muxer.

## 5. The two engines differ, and it is declared

`MetadataSupport` is a per-backend claim, defaulting to `false` for every field,
so an engine has to *claim* a capability rather than inherit it. That is what lets
the CLI say "this engine dropped it" instead of a caller guessing from an empty
output.

| | Engine B (our muxer) | Engine A (ImageIO) |
|---|---|---|
| Exif | ✅ byte-identical | ✅ ImageIO's re-serialization |
| XMP | ✅ | ✅ |
| IPTC-IIM | ✅ | ❌ |
| opaque items | ✅ | ❌ |

**Engine A writes no arbitrary items.** There is no ImageIO call that adds an
`infe`/`iloc` pair to a HEIF file, so Apple's 110-key Photographic Styles plist
can only be carried by our own muxer. `--engine videotoolbox` keeps it; the default
`--engine apple` warns and drops it.

**Engine A writes no IPTC**, and that is measured rather than inferred from an
API. ImageIO *reads* the IIM block back out of a carrier — 8 entries, confirmed on
both a TIFF and a JPEG by `probe_exif_props.rs` — and its HEIC writer then emits
no IPTC at all. Handing it the dictionary is necessary and not sufficient. Where
the source also put those fields in XMP, as Lightroom does, the information
survives anyway; the IIM encoding of it does not.

**Two ImageIO surprises worth knowing**, both measured, both worked around:

- `CGImageSource` will not read a pixel-less TIFF, so an Exif block has to be
  wrapped in a decodable 1×1 JPEG before ImageIO will parse it: bare block →
  `count=0`, no properties; wrapped → 32 Exif, 9 TIFF, 15 GPS.
- `CGImageMetadataCreateFromXMPData` **rejects a packet whose XML attributes are
  single-quoted** — legal XML, and exiftool's default — returning NULL with no
  diagnostic. The identical packet parses through the *file-level* reader, so the
  fallback wraps it in the same kind of carrier. Converting the packet's quotes
  from `'` to `"` also fixes it, which is how the cause was isolated.

Engine A's Exif is therefore ImageIO's re-serialization, not the source's bytes.
It agreed tag-for-tag with Engine B on every group measured, but it is a round
trip through a black box and only tags ImageIO knows survive it.

## 6. Lightroom does export it

Worth stating because the obvious first guess is that LrC strips metadata and it
does not. From `com.adobe.LightroomClassicCC7.plist`:

```
AgExport_embeddedMetadataOption   = "all"
AgExport_minimizeEmbeddedMetadata = false
AgExport_removeLocationMetadata   = true      <- GPS, stripped by LrC
AgExport_removeFaceMetadata       = true
```

So a LrC TIFF arrives with the full set **minus GPS and face regions**, which LrC
removes before we see the file. The plugin's `updateExportSettings` sets format,
colour space, bit depth and compression and says nothing about metadata, so it
inherits whatever the Export dialog holds. A plugin that wanted GPS back would
have to force `LR_removeLocationMetadata = false` — and should probably ask first,
since the user set that deliberately.

Caveat: this is the dialog's remembered state, not a measured export. No LrC TIFF
has been through the `tiff-ifd0` path yet; that arm is tested against a
`tools/make_hdr_source.py` TIFF with `exiftool`-injected tags, not against
Lightroom's own output.

## 7. Still missing

Three things, and only the last is a passthrough gap.

**ICC profiles (4 in the reference capture, 113 exiftool tags).** Not a passthrough gap but a
colour-authority one: the output states its colour in `colr` as `nclx`, and a
second statement that disagreed would be unresolvable. Carrying Apple's
26,664-byte "Display P3 Primaries; PQ (Adaptive Gain Curve)" profile onto our
`tmap` would mean asserting it describes *our* reconstruction. That is the
`colr`/ICC question in `heic-gainmap-structure.md` §7, and the place Engine A
still writes `prim=unspecified`.

**Auxiliary images: 5 of 6, plus the `thmb` thumbnail.**
`semanticskymatte`, `styledeltamap` (itself a 48-tile grid),
`portraiteffectsmatte`, `semanticskinmatte`, `linearthumbnail`. This is image
data, not metadata: each needs its coded bitstream, `hvcC`, `ispe`, `auxC` and —
for the grid — its 48 constituent items. They are also what the `QuickTime` group
count (125 → 63) and the four dropped XMP items are about. Camera-capture
artifacts, so no LrC TIFF has them; for a HEIC→HEIC conversion the mattes stay
geometrically valid, which makes copying them a real option and a separate
decision.

**Engine A's two gaps**, §5: the Photographic Styles plist and IPTC-IIM. The
first is an ImageIO limitation with no workaround; the second is an ImageIO writer
limitation. `--engine videotoolbox` has neither.
