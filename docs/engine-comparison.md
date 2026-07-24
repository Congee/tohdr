# Engine A vs Engine B

Two engines produce gain-map HEICs from the same derived inputs:

- **Engine A — `apple-imageio`.** Links CoreGraphics/ImageIO and attaches the
  base and gain plane through `CGImageDestination::add_auxiliary_data_info`.
- **Engine B — `portable-hpvca`.** `hpvca` (BSD-3/Apache) encodes the two
  planes; `tohdr-heif`, written for this project, muxes them.

macOS ImageIO is the correctness oracle throughout: it is the decoder every
Apple app actually goes through, so its opinion is the one that decides whether
a file renders as HDR. Our own reader is never used to score our own writer.

Every number below was measured on this machine, at the commit that added this
file. Reproduce with `tohdr bench` and
`cargo run --release --example roundtrip -p tohdr-apple`.

## Correctness

Both engines pass **9 of 9 applicable acceptance criteria, exit 0**, on the
independent `tools/verify_gainmap.py` (which shares no code with the Rust
crates). Criteria 8 and 9 skip for both: neither writes MakerApple tags or an
XMP headroom copy yet, which is the remaining gap against `IMG_4913.HEIC`'s
11/11.

What ImageIO reports for each engine's output:

| | Engine A | Engine B | IMG_4913 |
|---|---|---|---|
| `apple_aux` | yes | yes | yes |
| `iso_aux` | yes | yes | yes |
| gain pixel format | `L008` | `L008` | `L008` |
| `headroom_consistent` | true | true | true |
| images enumerated | 1 | 1 | 1 |

Engine B only reaches that row after the `grpl`/`altr` fix — see
[the structure notes](heic-gainmap-structure.md). Before it, ImageIO reported
`iso_aux=false` and enumerated two images for a `tmap` that our reader and the
Python checker both called valid. That defect was invisible to everything
except the platform oracle, which is the argument for having one.

### Where Engine A is not a faithful oracle of itself

`tohdr_apple::encode_from_hdr` (ImageIO authoring the whole file from an HDR
`CGImage`, via `kCGImageDestinationEncodeToISOGainmap`) produced a file with
**zero declared headroom** — `alt_headroom = 0.0`, `max_log2 = 0.0` — from a
source with a real 2.368x peak. The container was well formed and ImageIO read
it back happily; the gain map simply carried nothing. Not yet diagnosed. It is
why `AppleEngine::encode` uses `encode_parts` with our own derived plane rather
than delegating derivation to ImageIO.

## Performance

`tohdr bench`, release build. The source is decoded and the gain map derived
**once** and both engines encode those identical bytes, so the numbers isolate
encode-and-mux. The shared load+derive cost is reported separately rather than
folded in.

| Source | Shared load+derive | Engine A | Engine B |
|---|---|---|---|
| 1024×768 (0.79 MP), 10 iters | 46.7 ms | 80.6 ms mean, 9.8 MP/s | **69.0 ms mean, 11.4 MP/s** |
| 3024×4032 (12.19 MP), 5 iters | 706.5 ms | **228.4 ms mean, 53.4 MP/s** | 595.1 ms mean, 20.5 MP/s |

**They cross over.** Engine B wins on the small image, where ImageIO's
fixed framework overhead dominates. Engine A wins by 2.6x on the 12 MP image
and *speeds up* with size (9.8 → 53.4 MP/s), which is what a hardware-assisted
encoder looks like; Engine B stays roughly flat (11.4 → 20.5 MP/s). Anyone
choosing an engine on speed should choose by image size, not in general.

Note the shared load+derive at 12 MP (706 ms) exceeds either engine's encode
time. The derivation, not the codec, is the bottleneck on large images — it is
scalar Rust and has had no optimization attention.

## Output size, and why the raw number misleads

At the same nominal `quality` 85, Engine B's files are roughly 2.3x smaller
(14,584 vs 34,012 bytes at 0.79 MP; 79,891 vs 190,159 at 12 MP). That is **not**
evidence Engine B compresses better: the two encoders map a 1..=100 quality
scale onto their own quantizers however they like, and nothing requires 85 to
mean the same thing to both.

So the reconstruction was measured instead — encode with each engine, decode
back **through ImageIO with the gain map applied**, compare to the source HDR:

| | Engine A | Engine B |
|---|---|---|
| bytes | 34,012 | 14,584 |
| decoded peak | 2.368x (exact) | 2.368x (exact) |
| PSNR | 68.84 dB | 68.13 dB |
| p99.9 relative luma error | 3.29% | 3.10% |
| worst relative luma error | 12.26% | **42.08%** |

Both reconstruct the peak exactly, and both clear criterion 12's 40 dB PSNR bar
by ~29 dB. Engine B is genuinely getting 2.3x the compression for 0.7 dB of
PSNR — a real win. What it pays is in the **tail**: 42% worst-case error against
Engine A's 12%.

That tail is not spread over the image. p99.9 error is ~3% for both, so it is
about one pixel in a thousand, and the worst pixel for *both* engines sits at
saturation 0.85 — inside the saturated red highlight that
`tools/make_hdr_source.py` puts there deliberately. A single-channel gain map is
derived from luma, so a highlight that clips in one channel only is
under-corrected by construction. Both engines hit that limit; Engine B hits it
harder because it quantizes the gain plane more aggressively.

This is a property of 1-channel gain maps, not of either encoder. The fix, if it
matters for a given image, is a 3-channel map — permitted by ISO 21496-1 and
already supported by our serializer (141-byte payload).

## On a real photograph

Everything above uses the synthetic source. The end-to-end run that matters most
is the user's own capture, `IMG_4913.HEIC` (24.5 MP), decoded by ImageIO with
its gain map applied, tone-mapped and re-derived by us, and re-encoded:

```
tohdr convert IMG_4913.HEIC -o real_apple.heic --engine apple --flavor both
  -> 2,377,792 bytes, headroom 2.268 stops, 3.2 s wall clock
```

| | Apple's original | our re-derivation |
|---|---|---|
| image | 5712×4284 | 5712×4284 |
| gain plane | 2856×2142 `L008` | 2856×2142 `L008` |
| declared headroom | 2.287109 stops | 2.268141 stops |
| `max_log2 == alt_headroom` | yes | yes |

The headroom numbers agree to **0.019 stops (0.8%)** despite being produced by
completely separate code — Apple's camera pipeline on one side, our
`derive_consistent` on the other, from a decode of Apple's own reconstruction.
The gain-plane geometry lands on exactly Apple's half-resolution convention.
`tools/verify_gainmap.py`: 9 passed, 0 failed, exit 0; ImageIO reports both
flavors present.

Engine B cannot be run on this input directly — its pure-Rust decoders take
TIFF/PNG/JPEG, not HEIC, and it reports that rather than guessing:

```
error: loading /…/IMG_4913.HEIC: unsupported extension Some("heic")
       (want tif/tiff/png/jpg/jpeg)   [exit 1, no file written]
```

## Caveats

- One machine, one run per configuration. Means over 10 and 5 iterations
  respectively; min/max are in the `tohdr bench --json` output.
- The synthetic source is smooth and compresses far better than a photograph.
  Absolute file sizes here are not representative; the *ratio* between engines
  is the transferable part, and even that is scene-dependent.
- `roundtrip` uses `DeriveOptions::default()` (full-resolution gain plane),
  while `tohdr convert` defaults to `--gain-subsample 2`. Consistent across both
  engines, so the comparison holds, but the absolute sizes differ from the CLI's.
- Neither engine has been tested on the iOS WeChat app, the symptom that
  started this project. That remains a device test nobody has run.
