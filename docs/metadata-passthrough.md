# Metadata Passthrough — what a conversion keeps, and what it cannot

Until this was written, a conversion produced a file with **no Exif at all**: no
camera, no lens, no exposure, no date, no location. That was never a decision —
it was three separate gaps lining up, and this document records what each one
was, what closed it, and what is still missing.

## 1. Where it used to go

The loss had three independent causes, and closing any two of them alone would
have changed nothing.

1. **No loader read it.** `tohdr_portable::gainmap_tiff` walks `IFD0` → tag 330
   → the gain SubIFD → tag 52557 and the pixel strips. It never looked at tag
   `0x8769` (the Exif IFD pointer) or the GPS IFD. `tohdr_apple::read` did call
   `properties_at_index`, but only to fish out the HDRToneMap values.
2. **Nothing could carry it.** `GainMapEncoder::encode` took `(base, gain, meta,
   opts)`, and `GainMapMeta` is gain-map fields only. `Rgb`/`HdrRgb` are
   `{width, height, bits, data}`. The pipeline was pixels-only end to end, so a
   loader that *had* read Exif would have had nowhere to put it.
3. **Both writers declined.** `tohdr_heif`'s muxer already implemented the `Exif`
   item completely — `MuxRequest::exif`, the `infe`, the `idat` payload with its
   4-byte `exif_tiff_header_offset`, the `cdsc` reference — and
   `engine.rs` passed `exif: None`. Engine A built a `CGImageMetadata` for the
   headroom XMP and never built an Exif dictionary; it could not fall back on
   ImageIO either, because a `CGImage` made from raw pixel bytes carries no
   metadata to copy.

## 2. What it does now

`EncodeOptions::exif` carries a standalone Exif TIFF block, borrowed rather than
owned so the type stays `Copy` — `encode_within_budget` derives a fresh options
struct per quality it tries, and a 3 KB block should not be cloned once per
binary-search step. `GainMapEncoder::carries_exif` defaults to `false`, so a
backend has to *claim* the capability; that is what lets the CLI distinguish
"the source had no Exif" from "this engine dropped it" instead of a caller
guessing from an empty output.

Measured on `IMG_4913.HEIC` (5712×4284, iPhone 17 Pro), same scene through both
engines:

| tag group | IMG_4913 | Engine A | Engine B |
|---|---|---|---|
| `ExifIFD` | 32 | 32 | 32 |
| `GPS` | 15 | 15 | 15 |
| `IFD0` | 9 | 8 | 8 |
| `Apple` (MakerNote) | 25 | 0 | 0 |
| `PLIST` | 110 | 0 | 0 |
| ICC profiles | 4 | 0 | 0 |

The one `IFD0` tag we decline is `Orientation`; §4 says why. Cost in bytes:
2,114,582 → 2,115,765, so 55 tags for 1,183 bytes.

All three source containers are supported, and the CLI reports which one it
found (`exif_source` in `--json`):

| source | where the block lives | reported as |
|---|---|---|
| HEIF/HEIC | the `Exif` item's payload, past `exif_tiff_header_offset` | `heif-exif-item` |
| JPEG | the first `APP1` segment tagged `Exif\0\0` | `jpeg-app1` |
| TIFF | the file's own `IFD0` — the Lightroom path | `tiff-ifd0` |

## 3. Lightroom does export it

Worth stating because the obvious first guess is that LrC strips metadata and it
does not. From `com.adobe.LightroomClassicCC7.plist`:

```
AgExport_embeddedMetadataOption   = "all"
AgExport_minimizeEmbeddedMetadata = false
AgExport_removeLocationMetadata   = true      <- GPS, stripped by LrC
AgExport_removeFaceMetadata       = true
```

So a LrC TIFF arrives with the full set **minus GPS and face regions**, which
LrC removes before we see the file. The plugin's `updateExportSettings` sets
format, colour space, bit depth and compression and says nothing about metadata,
so it inherits whatever the Export dialog holds. A plugin that wanted GPS back
would have to force `LR_removeLocationMetadata = false` — and should probably
ask first, since the user set that deliberately.

Caveat: this is the dialog's remembered state, not a measured export. No LrC
TIFF has been through the `tiff-ifd0` path yet; the arm is tested against a
`tools/make_hdr_source.py` TIFF with `exiftool`-injected tags (59 carried), not
against Lightroom's own output.

## 4. Why the block is rebuilt rather than copied

Copying the bytes is simpler and wrong four ways. `tohdr_portable::exif`
re-serializes instead: an allowlist of `IFD0` tags, the Exif and GPS sub-IFDs
copied entry by entry, values relocated into a fresh value area, everything
emitted in the *source's* byte order so no value ever needs byte-swapping. A
RATIONAL is two LONGs rather than one 8-byte quantity, and that distinction is
exactly what a byte-order conversion gets wrong, so the tests cover a big-endian
source specifically.

**Pixel-structure tags.** A TIFF source's `IFD0` describes pixels as well as
metadata. Its `StripOffsets` point into a file the output does not contain and
its `SubIFDs` pointer reaches the Lightroom gain map. Emitted verbatim, those
are live pointers into nothing. The allowlist is an allowlist and not a denylist
because the failure directions are not symmetric: a forgotten exclusion emits a
dangling offset, a forgotten inclusion merely loses a tag.

**`Orientation` (`0x0112`) is dropped.** Neither loader rotates pixels and the
muxer writes `irot(0)`, so the output's pixels are the source's stored pixels and
the container declares no rotation. A copied `Orientation` would be a second,
contradicting statement, and for a rotated source two conformant viewers would
then disagree about which way up the photo goes — HEIF says the container's
transform wins, Exif readers say the tag does. **A rotated source therefore still
comes out the way it went in, unrotated.** That is not a new regression; it is the
pre-existing state, now written down. Reading `Orientation` into `irot`/`imir` is
the real fix and is deliberately not bundled here.

**`MakerNote` (`0x927C`) is dropped.** Apple's — and most vendors' — maker notes
address their own contents with offsets relative to the *original* TIFF header,
so a relocated maker note no longer parses. Keeping it would trade a missing
block for a corrupt one. This also leaves `acceptance-criteria.md` §8 intact: we
still write no MakerApple headroom tags, so criteria 8 and 9 still skip rather
than reporting a value nothing should trust.

**An embedded ICC profile (`0x8773`) is dropped**, for the same one-authority
reason: the output states its colour in `colr`, and a second statement that
disagreed would be unresolvable. Carrying colour properly is the `colr`/ICC
question in `heic-gainmap-structure.md` §7, not this one.

## 5. The two engines get there differently

**Engine B** writes the `Exif` item directly, so the block lands byte-identical —
asserted in `mux_roundtrip.rs::an_exif_item_round_trips_byte_for_byte`, which
also checks the `exif_tiff_header_offset` prefix separately, because a muxer that
wrote the block and forgot the prefix would still round-trip through a naive
reader.

**Engine A** has no such door: ImageIO authors the whole file and takes metadata
only as `kCGImageProperty*Dictionary` entries keyed by CF strings. The obvious
implementation is a table mapping every TIFF tag number onto its key — several
hundred lines that silently lose each tag Apple adds. Instead the block goes to
ImageIO's *reader* and the dictionaries come straight back to its writer.

That needs the block to be readable by `CGImageSource`, and
`examples/probe_exif_props.rs` establishes the shape of what works:

```
== the bare Exif block (3074 bytes, byte order "MM") ==
  CGImageSource: status=0 count=0
  no properties — ImageIO will not read a pixel-less TIFF
```

An Exif block *is* a TIFF, but one with no `StripOffsets`, and ImageIO finds no
image in it — `count=0`, no properties, at any status. Wrapped in a 1×1 baseline
JPEG (`tohdr_core::exif::wrap_in_jpeg`) the same block yields 32 Exif, 9 TIFF and
15 GPS entries. The wrapper must be a *decodable* image, not just a header.

The consequence to remember: Engine A's output carries ImageIO's
re-serialization, not the source's bytes. It agreed tag-for-tag with Engine B on
`IMG_4913.HEIC`, but it is a round-trip through a black box and only tags ImageIO
knows survive it.

## 6. Still missing

- **`Apple` MakerNote (25 tags)** — dropped on purpose, §4.
- **The Apple plist item (110 tags)** — a `uri` item named `metadata`; not read.
- **5 of 6 auxiliary images** — `semanticskymatte`, `styledeltamap`,
  `portraiteffectsmatte`, `semanticskinmatte`, `linearthumbnail`. Camera-capture
  artifacts; no LrC TIFF has them, so for the plugin path there is nothing to
  lose. For a HEIC→HEIC conversion they are in the source and the mattes would
  still be geometrically valid, which makes copying them a real option and a
  separate decision.
- **The `thmb` thumbnail item.**
- **All 4 ICC profiles**, including the 26,664-byte "Display P3 Primaries; PQ
  (Adaptive Gain Curve)" profile on the `tmap`. Not a passthrough gap — a
  deliberate `nclx`-instead-of-`prof` choice, and the place where Engine A still
  writes `prim=unspecified`.
- **IPTC-NAA (`0x83BB`)** — not in the `IFD0` allowlist. LrC writes IPTC into XMP
  as well, and we do not yet carry the source's XMP either (our XMP item holds
  only the headroom packet), so creator/copyright survive today only through
  `IFD0`'s `Artist` and `Copyright`.
