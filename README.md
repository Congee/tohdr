# tohdr

Produce HDR gain-map HEICs that actually render as HDR — verified against
Apple's own decoder, not against our own parser.

The project started from a concrete failure: two exported HEICs
(`DSC07752.heic`, `DSC07752_iso.heic`) looked washed out in the iOS WeChat app,
while an iPhone 17 Pro capture (`IMG_4913.HEIC`) looked right. Both are HEIC,
both carry a gain map. [`docs/heic-gainmap-structure.md`](docs/heic-gainmap-structure.md)
is the byte-level teardown of why they differ, and
[`docs/acceptance-criteria.md`](docs/acceptance-criteria.md) turns "as good as
IMG_4913" into 16 machine-checkable criteria.

## Quick start

```sh
cargo build --release -p tohdr-cli

# 12 MP synthetic HDR source with known-exact content
python3 tools/make_hdr_source.py out/scene.tiff --width 3024 --height 4032

# both gain-map flavors, capped at 4 MB
./target/release/tohdr convert out/scene.tiff -o out/photo.heic \
    --flavor both --max-size 4MB

./target/release/tohdr inspect out/photo.heic
./target/release/tohdr verify  out/photo.heic
./target/release/tohdr bench   out/scene.tiff
```

`--flavor apple | iso | both` chooses the signaling (`ios` is accepted as an
alias for `iso`). `--engine apple | portable` chooses the backend.

## Two engines

| | Engine A `apple-imageio` | Engine B `portable-hpvca` |
|---|---|---|
| Codec | Apple ImageIO | `hpvca` (BSD-3/Apache) |
| Container | ImageIO | `tohdr-heif`, written here |
| Apple frameworks | required | none |
| 0.79 MP | 80.6 ms | **69.0 ms** |
| 12.19 MP | **228.4 ms** | 595.1 ms |

They cross over — see [`docs/engine-comparison.md`](docs/engine-comparison.md)
for the measurements, including a round-trip quality comparison that makes the
file-size difference meaningful rather than misleading.

## Verifying

Three independent layers, deliberately not sharing code:

- `tohdr verify` — our own reader.
- `tools/verify_gainmap.py` — stdlib-only ISOBMFF walk plus `exiftool`, sharing
  no code with the Rust crates, so a bug present in both our reader and our
  writer still shows up.
- `tohdr_apple::inspect` — **macOS ImageIO itself**, the decoder every Apple app
  goes through.

That third layer earned its place. Engine B once produced a `tmap` our reader
and the Python checker both called valid, while ImageIO reported no ISO gain map
at all. The cause was a missing `grpl`/`altr` entity group; nothing but the
platform oracle could have caught it.

The second layer needed auditing too. Adversarial review found that criteria 1
and 4 could pass *vacuously* — the "base" item was derived from `dimg[0]` and
then compared against `dimg[0]`. Both now compare against `pitm`, which lives
in a different box, and the fix is demonstrated with a crafted file whose
`dimg` is reversed to `[gain, base]`: it fails, where the old checker passed
it.

Current status against the criteria:

| File | Result |
|---|---|
| `IMG_4913.HEIC` (reference) | 11 passed, 0 failed |
| Engine A output | 10 passed, 0 failed, 1 skipped |
| Engine B output | 10 passed, 0 failed, 1 skipped |
| `DSC07752_iso.heic` | **4 failed** |
| `DSC07752.heic` | **1 failed** |

The remaining skip is criterion 8, MakerApple tags 33/48, which neither engine
writes — deliberately. Apple's tag formula cannot express more than 3.0 stops
without a negative `tag48`, which is exactly how `DSC07752.heic` broke; clamping
instead makes the tags disagree with the ISO payload and fails criterion 9.
Writing nothing is the only option that never states a wrong headroom. See
criterion 8 in [`docs/acceptance-criteria.md`](docs/acceptance-criteria.md).

## Lightroom Classic plugin

`lightroom/tohdr.lrplugin` exports gain-map HEICs by shelling out to this CLI,
with flavor, engine, quality, tone map, and a maximum output size in the export
dialog. Its logic is unit-tested with a stock Lua interpreter, but **it has never
been run inside Lightroom** — see [`lightroom/README.md`](lightroom/README.md)
for exactly what is and is not verified.

## Layout

```
crates/tohdr-core      pixel types, derivation, ISO 21496-1, Apple tags, XMP
crates/tohdr-heif      ISOBMFF reader and gain-map muxer  (no unsafe)
crates/tohdr-apple     Engine A: ImageIO encode/decode/inspect  (the oracle)
crates/tohdr-portable  Engine B: hpvca + our muxer
crates/tohdr-cli       convert / inspect / verify / bench
tools/                 independent verifier, HDR test-source generator
lightroom/             Lightroom Classic export plugin and its tests
docs/                  structure teardown, acceptance criteria, engine comparison
```

## Known gaps

- **The original symptom is unconfirmed.** Nobody has opened any output of this
  project in the iOS WeChat app. Every check here is necessary; only a device
  test is sufficient.
- `tohdr_apple::encode_from_hdr` — letting ImageIO author the file from an HDR
  image — emits zero declared headroom from a source with real headroom. A
  sweep over four pixel layouts and two colour spaces rules out the pixel
  format; the likely cause is a content-headroom declaration that
  `objc2-core-graphics` 0.3.2 provides no way to set. It is why
  `AppleEngine::encode` uses our own derived plane instead.
- The gain-map derivation is scalar and unoptimized; at 12 MP it costs more
  than either encoder.
