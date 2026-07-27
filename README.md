# tohdr

Produce HDR gain-map HEICs that actually render as HDR — verified against
Apple's own decoder, not against our own parser.

A HEIC can carry a gain map and still render washed out instead of HDR. Whether
it works depends on container details that no error message reports: which items
exist, how they reference each other, and whether the declared headroom agrees
with what the gain map actually encodes.

Three real files anchor the work — an iPhone 17 Pro capture that renders as HDR
everywhere, and two third-party exports of one scene that render washed out, one
with Apple-style signaling and one with ISO 21496-1 signaling.
[`docs/heic-gainmap-structure.md`](docs/heic-gainmap-structure.md) is the
byte-level comparison of what differs between them, and
[`docs/acceptance-criteria.md`](docs/acceptance-criteria.md) turns "matches the
capture that works" into 16 machine-checkable criteria.

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

# a whole folder, several files at a time
./target/release/tohdr batch ~/Pictures/shoot -o out/hdr --max-size 4MB
```

`--flavor apple | iso | both` chooses the signaling (`ios` is accepted as an
alias for `iso`). `--engine apple | videotoolbox | hpvca` chooses the backend.

## Two engines, three backends

Engine A hands the planes to Apple's ImageIO. Engine B assembles the container
itself — `tohdr-heif`, written here — behind a swappable
[`PlaneCodec`](crates/tohdr-heif/src/engine.rs), so the HEVC encoder is a
substitutable part rather than the design. Two codecs implement it, hence three
selectable backends.

| | Engine A | Engine B | Engine B |
|---|---|---|---|
| `--engine` | `apple` | `videotoolbox` | `hpvca` |
| Reports itself as | `apple-imageio` | `hardware-videotoolbox` | `portable-hpvca` |
| Codec | Apple ImageIO | platform media block | `hpvca` (BSD-3/Apache) |
| Container | ImageIO | `tohdr-heif` | `tohdr-heif` |
| Apple frameworks | required | required | none |
| 0.79 MP | 77.7 ms | **10.1 ms** | 29.5 ms |
| 12.19 MP | 225.5 ms | **28.3 ms** | 257.4 ms |
| 60.22 MP | 545.3 ms | **221.3 ms** | 5762.5 ms |

`--engine videotoolbox` means "our container, the fastest codec this machine
has" — the media block when the job allows it, `hpvca` when it does not (a
10-bit base, or a quality that asks for 4:4:4 chroma). `--engine hpvca` forces
the pure-Rust path, which is how the two are compared. Only `hpvca` is portable:
the other two need macOS, and the `videotoolbox` path hands camera RAW to
ImageIO's decoder besides. The choice is made before encoding, not by recovering
from a hardware error, so a benchmark cannot be invalidated by a silent
substitution.

Those timings are the mean of iterations 2..n with the `VTCompressionSession`
reused — what a *second* file of the same geometry costs. Cold, in a fresh
process, the media block pays a start-up that dominates at small sizes: 36.6 /
102.7 / 430.7 ms on the same three fixtures, which is slower than `hpvca` at
0.79 MP. [`docs/engine-comparison.md`](docs/engine-comparison.md) has the
measurements, the session-pool limits, and a round-trip quality comparison that
makes the file-size difference meaningful rather than misleading.

A 60 MP raw converts in 2.6 s and a folder of them at 48 files/min;
[`docs/performance.md`](docs/performance.md) has the stage breakdown, the
architecture behind it, and the optimisations that measured worse.

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

The second layer needed auditing too. Criteria 1 and 4 could pass *vacuously* —
the "base" item was derived from `dimg[0]` and then compared against `dimg[0]`.
Both now compare against `pitm`, which lives in a different box, and the fix is
demonstrated with a crafted file whose `dimg` is reversed to `[gain, base]`: it
fails, where the old checker passed it.

Current status against the criteria. The first row is the iPhone capture that
renders correctly; the last two are the washed-out exports, which is what the
criteria have to be able to reject:

| File | Result |
|---|---|
| iPhone 17 Pro reference capture | 11 passed, 0 failed |
| `apple-imageio` output | 10 passed, 0 failed, 1 skipped |
| `hardware-videotoolbox` output | 10 passed, 0 failed, 1 skipped |
| `portable-hpvca` output | 10 passed, 0 failed, 1 skipped |
| washed-out ISO-flavor export | **4 failed** |
| washed-out Apple-flavor export | **1 failed** |

The remaining skip is criterion 8, MakerApple tags 33/48, which neither engine
writes — deliberately. Apple's tag formula cannot express more than 3.0 stops
without a negative `tag48`, which is exactly how the Apple-flavor export broke; clamping
instead makes the tags disagree with the ISO payload and fails criterion 9.
Writing nothing is the only option that never states a wrong headroom. See
criterion 8 in [`docs/acceptance-criteria.md`](docs/acceptance-criteria.md).

## Lightroom Classic plugin

`lightroom/tohdr.lrplugin` exports gain-map HEICs by shelling out to this CLI,
with flavor, engine, quality, tone map, and a maximum output size in the export
dialog. It runs inside Lightroom Classic 15.4.1: a 60.2 MP Sony raw exports to a
gain-map HEIC that passes `tohdr verify`. Its Lua logic is unit-
tested with a stock interpreter as well. See
[`lightroom/README.md`](lightroom/README.md) for what that live run established
— including that Lightroom's Lua sandbox has no `os.getenv` — and what is still
open.

## Layout

```
crates/tohdr-core      pixel types, derivation, ISO 21496-1, Apple tags, XMP
crates/tohdr-heif      ISOBMFF reader and Engine B's muxer  (no unsafe)
crates/tohdr-apple     Engine A: ImageIO encode/decode/inspect  (the oracle),
                       and Engine B's VideoToolbox plane codec
crates/tohdr-portable  Engine B's hpvca plane codec, and the pure-Rust decoders
crates/tohdr-cli       convert / batch / inspect / verify / bench
tools/                 independent verifier, HDR test-source generator
lightroom/             Lightroom Classic export plugin and its tests
docs/                  structure teardown, acceptance criteria, engine comparison,
                       performance
```

## Known gaps

- **No output has been checked in a closed-source third-party viewer.** The
  washed-out rendering that motivated this work was seen in one such app (iOS
  WeChat), and no output of this project has been opened there. Every check here
  is necessary; only viewing on a device is sufficient.
- `tohdr_apple::encode_from_hdr` — letting ImageIO author the file from an HDR
  image — emits zero declared headroom from a source with real headroom. A
  sweep over four pixel layouts and two colour spaces rules out the pixel
  format; the likely cause is a content-headroom declaration that
  `objc2-core-graphics` 0.3.2 provides no way to set. It is why
  `AppleEngine::encode` uses our own derived plane instead.
- Engine B reads a 16-bit TIFF as PQ regardless of the profile the file
  carries, so a gamma-encoded 16-bit export — what Lightroom produces — comes
  out with invented headroom (5.6 stops against Engine A's 0.04 on the same
  file). The assumption is documented in `tohdr_portable::input`, and Engine A
  reads the embedded profile correctly; the two engines simply disagree about
  what an unlabelled 16-bit TIFF means.
- Raw files converted through the CLI ignore any Lightroom sidecar. ImageIO
  does not read `.xmp`, verified by converting a raw with and without its
  sidecar present and getting the same bytes. Use the Lightroom plugin, which
  is handed an already-developed image, or export a TIFF first.

## License

Dual-licensed under either

- Apache License 2.0 ([`LICENSE-APACHE`](LICENSE-APACHE)), or
- MIT license ([`LICENSE-MIT`](LICENSE-MIT))

at your option. Apache-2.0 is offered because this is codec-adjacent work: it
carries an express patent grant from each contributor, which MIT does not. That
grants only patents a contributor actually holds — neither license says anything
about third-party patents, and HEVC in particular is pool-licensed by others.

Unless you state otherwise, any contribution you submit for inclusion is dual-
licensed as above, with no additional terms.

Every dependency in the resolved graph is permissive — MIT, Apache-2.0, BSD-2,
BSD-3, 0BSD, Zlib, Unicode-3.0 or Unlicense — enforced by `cargo deny` against
[`deny.toml`](deny.toml). Note that `cargo deny` does not walk dev-dependency
subtrees, so it will not catch a copyleft crate arriving that way; that is why
`ultrahdr-core` is pinned to `default-features = false`.
