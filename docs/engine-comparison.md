# Engine A vs Engine B

Two engines produce gain-map HEICs from the same derived inputs:

- **Engine A — `apple-imageio`.** Links CoreGraphics/ImageIO and attaches the
  base and gain plane through `CGImageDestination::add_auxiliary_data_info`.
- **Engine B — our muxer plus a swappable plane codec.** `tohdr-heif`, written
  for this project, assembles the container; the HEVC encoder behind it is a
  [`PlaneCodec`](../crates/tohdr-heif/src/engine.rs) implementation:
  - `hardware-videotoolbox` — the platform media block. What `--engine portable`
    selects when the job allows it.
  - `portable-hpvca` — `hpvca` (BSD-3/Apache), pure Rust, no Apple frameworks.
    The fallback, and what `--engine hpvca` forces.

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
`CGImage`, via `kCGImageDestinationEncodeToISOGainmap`) produces a file with
**zero declared headroom** — `alt_headroom = 0.0`, `max_log2 = 0.0` — from a
source with a real 2.368x peak. The container is well formed and ImageIO reads
it back happily; the gain map simply carries nothing. This is why
`AppleEngine::encode` uses `encode_parts` with our own derived plane rather
than delegating derivation to ImageIO.

Two separate bugs were tangled here, and separating them took a controlled
sweep over pixel layouts:

| `CGImage` layout | file | decoded base mean | declared headroom |
|---|---|---|---|
| 96 bpp RGB, `AlphaNone`, linear sRGB | 34,140 B | **66.8/255** | 0.0000 |
| 128 bpp RGBA, `PremultipliedLast`, linear sRGB | 11,655 B | 161.4/255 | 0.0000 |
| 128 bpp RGBX, `NoneSkipLast`, linear sRGB | 11,655 B | 161.4/255 | 0.0000 |
| 128 bpp RGBX, `NoneSkipLast`, linear P3 | 12,018 B | 160.0/255 | 0.0000 |

**Fixed:** the 96 bpp row is a genuine defect — CoreGraphics has no valid
alpha-less 96 bpp RGB float layout, `CGImageCreate` accepts the arithmetic
anyway, and the buffer is then misread. Its decoded base is nowhere near the
other three and its file is 3x larger for identical content.
`cg_image_from_hdr` now uses 128 bpp `NoneSkipLast`, matching what the read
side already did; `encode_from_hdr`'s output dropped from 34,140 to 11,655
bytes accordingly.

**Still open:** the headroom is zero in *every* layout and both colour spaces,
so the pixel format was never its cause. The likely explanation is that
`kCGImageDestinationEncodeToISOGainmap` wants the source image to declare a
content headroom, and `objc2-core-graphics` 0.3.2 exposes no way to set one —
it binds `CGContextSetEDRTargetHeadroom` but nothing equivalent for a
`CGImage`. Untested, because the API is not reachable from here.

## Performance

`tohdr bench`, release build. The source is decoded and the gain map derived
**once** and both engines encode those identical bytes, so the numbers isolate
encode-and-mux.

Every figure below is the mean of iterations 2..n — what a *second* file of the
same size costs. That distinction is not pedantry: on both Apple paths the first
encode of a given geometry pays start-up that nothing after it pays, and on the
media block that start-up is most of the work. `bench` therefore reports `first`
and `then` separately, and the columns here say which regime they are in.

| Source | Engine A | B hw, own session per file | B hw, session reused | B `portable-hpvca` |
|---|---|---|---|---|
| 512×384 (0.20 MP) | 68.1 ms | 35.7 ms | **5.4 ms** | 20.4 ms |
| 1024×768 (0.79 MP) | 77.7 ms | 36.6 ms | **10.1 ms** | 29.5 ms |
| 3024×4032 (12.19 MP) | 225.5 ms | 102.7 ms | **28.3 ms** | 257.4 ms |
| 9504×6336 (60.22 MP) | 545.3 ms | 430.7 ms | **221.3 ms** | 5762.5 ms |

Fixtures: `out/tiny.tiff`, `out/small.tiff`, `out/scene.tiff`, and a 60 MP export
of `DSC07746.ARW`. The last is 722 MB and is not kept in the tree; regenerate it
with `cargo run --release --example export_hdr_tiff -p tohdr-apple -- <raw>
out/big60.tiff`, which verifies the round trip is bit-identical before returning.
(An earlier version of this table used fixtures that no longer exist, which is
why the exporter exists at all. Its absolute numbers differed — the sources were
different photographs — so the whole table was re-measured rather than patched.)

Reading it:

- **The media block wins at every size, once its session is reused** — 0.08x
  Engine A at 0.20 MP, 0.13x at 12 MP, 0.41x at 60 MP.
- **Give it a fresh session per file and it wins only above ~1 MP.** That is the
  cold column, and it is what a single `tohdr convert` in a fresh process gets:
  0.52x Engine A at 0.79 MP but *slower* than hpvca there, 20.4 ms against 36.6.
  This document used to state flatly that "below ~1 MP the software codec wins
  outright". That was true of the code as it stood and is now true only of the
  cold column: reuse moves the crossover below the smallest size measured.
- **The software codec collapses at scale**, 5.8 s at 60 MP against Engine A's
  0.55. That is not a parallelism failure; it is profiled below.
- **Engine A has its own warm-up** and it is not small — 629 ms first against
  545 ms after, at 60 MP — but it is framework initialisation, paid once per
  process, not once per geometry.

`--engine portable` picks between the two codecs per job. It selects the hardware
codec at every size, which the warm column now justifies at the small end too;
before, sub-megapixel inputs were a case of preferring one rate-distortion curve
over a small speed loss.

### Session reuse: what it is and what it is worth

Nothing about a `VTCompressionSession` depends on the pixels — only on geometry,
plane kind, quality and the `RealTime` flag — so `tohdr_apple::vtenc` keeps them
in a pool keyed on exactly that, and a folder of same-sized files shares two.
Per base plane at 12.19 MP (`examples/probe_vt_session_reuse.rs`):

```text
          session      fill    encode     total
  #0         25.7       5.1      66.4      97.1   <- created here
  #1          0.0       5.4      24.8      30.1   <- from the pool
  #2          0.0       4.7      22.4      27.0
```

Only 25.7 ms of the 70 ms saved is session creation. The rest hides inside
`encode_ms`, because VideoToolbox brings the encoder up lazily on the first
frame — so it presents as the cost of encoding rather than the cost of starting,
and an earlier note here that put the whole overhead at "30–45 ms, roughly 15% of
a 60 MP conversion" was `session_ms` mistaken for the total.

**It is byte-transparent.** A cold single `convert` and all 24 outputs of a pooled
batch share one SHA-256 (`d02e83e1…`), and `bench` reports identical output sizes
cold and warm at all four sizes above. That was the thing worth checking before
the milliseconds: an encoder carrying state between frames could easily have made
a file's bytes depend on its position in the batch, which is a worse property than
the speed is worth. What makes it hold is that every frame is an IDR with
`MaxKeyFrameInterval = 1` and no reordering, and an IDR slice header carries no
`pic_order_cnt_lsb` at all (H.265 §7.3.6.1) — so not even the frame's index can
reach the bitstream. Presentation timestamps do advance per frame, and are
container metadata that never enters it.

Peak RSS is unchanged to within 0.4%. The sessions exist during the encode
either way; the pool only keeps them alive between encodes.

**What reaches the wall clock depends on how much of the batch is encoding**, and
the two ends are far apart. Interleaved A/B, four repeats,
`tohdr batch --engine portable --no-session-reuse` against the default:

| batch | jobs | own session per file | reused | |
|---|---|---|---|---|
| 24 × 12.19 MP TIFF | 4 | 3.30 s | **2.74 s** | −17% |
| 24 × 12.19 MP TIFF | 1 | 4.96 s | **3.76 s** | −24% |
| 8 × 60.2 MP ARW | 1 | 18.86 s | **17.68 s** | −6% |
| 8 × 60.2 MP ARW | 4 | 12.29 s | 12.45 s | none |

A RAW batch is decode-bound — ImageIO's demosaic is ~1.15 s per file and
single-threaded — so the encode is a tenth of the work, and at four jobs the
saved milliseconds are simply filled by another file's decode. Which is the
argument for `tohdr batch` read backwards: when every core is already busy,
making one stage faster moves nothing. The TIFF rows are what it looks like when
the encode *is* the work.

Reuse is on regardless, because it costs nothing. `--no-session-reuse` on `batch`
and `bench` exists so the claim can be re-measured rather than believed.

### What made Engine B 2.1x faster, and where its ceiling is

Engine B used to be **16x** Engine A at 60 MP. Two changes closed half of that,
neither of which touched the codec:

- **SAO off** (`codec::config_for`). hpvca runs an analysis encode before the
  real one to drive Sample Adaptive Offset, and it is on by default. Turning it
  off was 2.5x on the encode and made files *smaller*, so it is not the usual
  speed-for-quality trade — see the A/B below.
- **Base and gain encoded concurrently** (`PortableEngine::encode`). They are
  independent, and neither saturates ten cores alone. Worth ~10% at a full-res
  gain plane and ~2x at the subsample-2 plane `convert` actually uses, where the
  gain encode hides entirely inside the base encode's tail.

Controlled before/after on one real 60 MP photograph, same file, same build
otherwise: **8875.9 ms → 4193.3 ms**, output 20,733,215 → 20,719,004 bytes. The
speed doubled and the file got marginally smaller.

The SAO trade, measured through ImageIO on the 12 MP source
(`examples/roundtrip`, gain plane at full resolution):

| | SAO on | SAO off |
|---|---|---|
| bytes | 80,457 | **76,051** |
| PSNR | 69.16 dB | 69.06 dB |
| p99.9 rel err | 2.21% | 2.40% |
| worst rel err | 23.83% | 27.09% |
| decoded peak | 2.368x exact | 2.368x exact |

0.10 dB for 5.5% fewer bytes and 2.5x the speed. The worst-pixel tail widens by
3 points, and it lands where the tail always lands for a 1-channel gain map — the
deliberately saturated highlight discussed under "Output size" below.

### Where the remaining 8x lives — profiled, not assumed

Sampled with `samply` over a full 60 MP `convert --engine portable`, symbolized
against the binary, aggregated by encoder stage over all 70 threads. Total
**30.83 CPU-seconds** in 3.93 s wall — 7.8x parallel on 10 cores, so this is
**not** a parallelism failure. It is a total-work problem: Engine A does the same
job in **4.06 CPU-seconds** because the encode runs on the media block instead of
the CPU. Engine B burns 7.6x more CPU work to produce the same file.

| stage | % CPU | s | data-parallel? |
|---|---|---|---|
| RDOQ + quantize + transform | 22.80 | 7.03 | yes |
| CABAC entropy coding | 22.53 | 6.95 | **no — serial bitstream** |
| intra prediction + mode search (SATD) | 16.19 | 4.99 | yes |
| RDO rate estimation (`CabacEstimator`) | 12.03 | 3.71 | yes |
| kernel / syscalls / page faults | 9.93 | 3.06 | n/a |
| our code (`tohdr_core`, muxer) | 7.73 | 2.38 | already is |
| libm / platform | 2.84 | 0.88 | n/a |
| CU partition search | 2.21 | 0.68 | yes |

The profile is flat — the hottest single function is
`cabac::residual::write_residual::<CabacEstimator>` at 11.6%, and nothing else
clears 10%. That is what a tuned codec inner loop looks like; there is no hot
spot to delete.

**On SIMD:** already done, upstream. hpvca ships NEON kernels for the transforms,
SATD, dequantization and CTU activity and enables them by default — `satd_neon`
and `neon::transform::inverse_pass` are both visible in the profile above. There
is no un-pulled SIMD lever here.

**On GPU:** the table bounds it. Summing every data-parallel row gives **53%** of
the work. Make all of it free — a perfect, zero-cost offload — and Amdahl caps
the whole-program speedup at `1/(1 - 0.53)` ≈ **1.9x**. Engine B needs **8x** to
reach Engine A, so even an ideal GPU implementation leaves it ~4x behind. And
1.9x is the optimistic bound, because two things fight a real implementation:

- **CABAC is 22.5% and strictly serial.** Context-adaptive arithmetic coding is a
  bit-at-a-time feedback loop; it cannot be parallelized, on a GPU or anywhere
  else. It alone floors the achievable speedup at 4.4x.
- **HEVC intra prediction reads *reconstructed* neighbours.** Mode decision →
  reconstruct → next block is a serial dependency chain at block granularity;
  WPP and tiles break it only at row and tile boundaries, which on this frame is
  ~99 CTU rows. A GPU wants tens of thousands of independent work items and would
  get ninety-nine, with a host round-trip per row.

So the honest conclusion for the *software codec*: **no amount of GPU or SIMD
work closes this gap.** Engine A is a fixed-function ASIC; hpvca is a software
encoder already SIMD-optimized and running at 78% parallel efficiency. Asking it
to match Engine A is asking software to match silicon at the one job silicon was
built for.

The conclusion that follows is not "Engine B is slow" but "**the codec is the
wrong thing to optimize**". Engine B's own contribution — the muxer — is ~0.1 ms
of a 4193 ms encode. So swap the codec, which is what the next section does.

### Engine B-hw: keep the muxer, encode on the media block

The profile says our muxer is free (~0.1 ms of 4193) and the codec is everything.
So swap only the codec. `tohdr_apple::vtenc` drives VideoToolbox directly and
hands the coded frame to `tohdr_heif::mux` unchanged — same muxer, same
container, hardware encode.

24.5 MP capture, all three paths in one process (`examples/probe_hw_planes.rs`,
which drives the shipping `MuxEngine` rather than a copy of it). One encode each,
so the hardware rows are cold — they include the session creation and first-frame
bring-up that a batch pays once and the table above strips out:

| | subsample 1 | subsample 2 (`convert`'s default) |
|---|---|---|
| Engine A (ImageIO encode + ImageIO mux) | 277.4 ms | 231.3 ms |
| Engine B, hpvca + our mux | 1396.0 ms | 949.4 ms |
| Engine B, VideoToolbox + our mux | **261.8 ms** | **187.8 ms** |
| hw vs A | 0.94x | 0.81x |
| hw vs hpvca | **5.33x faster** | **5.05x faster** |

**8x behind became 0.8x — ahead.** The output is valid where it matters: ImageIO
reports both flavors present, the gain plane comes back as `L008` — the HEVC
Monochrome profile keeps it genuinely single-channel, matching Apple's own
convention rather than acquiring neutral chroma — and `tools/verify_gainmap.py`
gives 0 failed / 9 passed / 2 skipped.

Three things that had to be measured rather than assumed:

- **ImageIO cannot be used as the plane encoder.** The cheap version of this idea
  is to ask ImageIO for a one-image HEIC per plane and pull the coded bytes back
  out, exactly as the hpvca path does. ImageIO tiles a 60 MP HEIC into a HEIF
  `grid`, and `coded_image` refuses a grid because reassembling tiles is a
  re-encode. Going straight to VideoToolbox skips the container round-trip
  entirely, which is also why it is the right shape for other platforms.
- **Overlapping the two plane encodes still helps**, even though the media block
  is one shared unit: 328.2 ms sequential (base 171.9 + gain 156.3) against
  261.8 ms concurrent *including* the mux, because one plane's CPU-side session
  setup and pixel fill overlap the other's hardware encode.
- **`RealTime=true` is the faster setting**, which is not the obvious choice for
  a still-image encoder. At 60 MP q85 it was 415.4 ms / 15,979,461 B against
  465.8 / 17,096,568 with it off. For a single all-intra frame the
  quality-oriented path spends its extra analysis on multi-frame decisions that
  cannot pay off. **Correction to an earlier reading of this:** it was recorded
  here as "faster *and* smaller, so no trade to make". Fewer bytes at the same
  requested quality is not free by itself — it is also what lower fidelity looks
  like. See the next subsection; the *reason* the file was smaller turned out not
  to be `RealTime` at all.

Letting VideoToolbox do the colour conversion — feeding it BGRA rather than
converting to 4:2:0 in Rust first — was worth 1050 → 867 ms and is the more
correct choice, since Apple applies a real matrix and range scaling instead of a
hand-rolled approximation. Which matrix, though, is the subject of the next
subsection, and getting that wrong cost 21 dB.

### The 21 dB bug that looked like a compression win

At the same `--quality`, the hardware path produced **half the bytes** of Engine
A at 12 MP (90,551 against 190,930). That reads like a better rate-distortion
curve. It was not: measured against the reconstruction it was **49.04 dB, against
Engine A's 70.06** (`examples/roundtrip`).

What ruled out compression as the cause was sweeping quality against fidelity
rather than against bytes (`examples/probe_vt_quality.rs`):

| requested quality | bytes | PSNR |
|---|---|---|
| 85 | 90,551 | 49.04 dB |
| 92 | 201,443 | 49.04 dB |
| 95 | 314,299 | 49.04 dB |
| 100 | 682,336 | 49.05 dB |

7.5x the bits for 0.01 dB. Quantization error responds to bitrate; this did not,
so the loss was systematic. The cause was the `colr` box: we hand VideoToolbox
BGRA and let *it* pick an RGB→YCbCr matrix, then the muxer declared a matrix of
its own — and a decoder applies the inverse of whatever the container claims.

One encode per codec, varying **only** the declaration
(`examples/probe_vt_colour.rs`):

| | declared BT.709 (matrix 1) | declared BT.601 (matrix 6) |
|---|---|---|
| VideoToolbox | **70.00 dB** | 49.04 dB |
| hpvca | 51.81 dB | **69.31 dB** |

The two codecs genuinely disagree, and the muxer had hard-coded BT.601 — correct
for hpvca, silently wrong for the media block. The fix is structural rather than a
constant: `PlaneCodec::base_colour` makes each codec declare the matrix it
actually wrote, with **no default implementation**, so a future VA-API or Vulkan
backend cannot inherit someone else's. Both values are pinned by unit tests.

After the fix, on the 12 MP source: Engine A 70.06 dB / 190,930 B, hardware
**69.35 dB / 90,551 B**, hpvca 69.06 dB / 76,051 B. Half Engine A's bytes at
0.7 dB less — the rate-distortion win is real once it is measured against the
image instead of against the byte count. The `full_range` flag turned out not to
matter to ImageIO's reconstruction either way.

**At matched fidelity the hardware path is 2.8x Engine A.** The comparison above
is at matched `--quality`, which leaves the hardware path 0.7 dB short. Raising it
until it *exceeds* Engine A costs almost nothing, because on this path bytes
respond to quality far more than time does:

| | ms | bytes | PSNR |
|---|---|---|---|
| Engine A, q85 | 273.8 | 190,930 | 70.06 dB |
| hardware, q85 | 162.2 | 90,551 | 69.35 dB |
| hardware, q100 | **97.8** | 682,336 | **70.23 dB** |

So the choice is 0.59x Engine A at 0.7 dB less and half the bytes, or **0.36x** at
0.2 dB *more* and 3.6x the bytes. `--quality` picks the point; the default stays
85 to match Engine A's flag semantics rather than its output size.

At 60 MP all three engines score an identical 18.77 dB, because that fixture
carries 5.45 stops of headroom and the reconstruction clamps at 2.90 — the metric
there measures the clamp, not the codec, so it separates nothing.

### Output is byte-reproducible, which took one deletion

Two encodes of identical pixels produced different files: exactly **two bytes** of
a 16.5 MB output, one per plane. Both sat inside a 59-byte prefix SEI of type
`user_data_unregistered`, UUID `4756 4adc 5c4c 433f 94ef c511 3cd1 43a8` —
VideoToolbox's private blob, carrying what looks like an encode-time counter. The
coded slices were bit-identical, so the *image* was always reproducible.

`vtenc::strip_unregistered_sei` drops it. That message carries no normative
decoding information by definition (H.265 §D.3.1), and ImageIO decodes the
stripped file to pixel-identical output — checked by decoding both to PNG and
comparing hashes. Output is now identical across runs, and 126 bytes smaller.

This matters because byte equality is how this project checks that a refactor did
not change output. A file that differs run to run for reasons unrelated to its
pixels makes that check useless.

Two smaller items remain, neither pulled:

- **`fill_bgra` is a whole extra pass.** `tone_map` writes `Rgb` as u16 (345 MiB),
  then the fill reads it and writes BGRA (241 MiB). Having the tone map emit a
  VT-ready buffer directly would delete a 345 MiB write and a 345 MiB read.
- **A fresh `VTCompressionSession` per plane per call**, 30–45 ms each and the
  reason the software codec still wins below a megapixel. Reusing one across
  planes and across files should matter most to `batch`.

### Cross-platform, which is the actual goal

`vtenc` is the macOS backend, not the design. Every fixed-function encoder hands
back the same thing — a raw HEVC bitstream plus parameter sets — which is exactly
`tohdr_heif::CodedImage`, so backends slot in beside it without the muxer
changing:

| platform | fixed-function encode reachable via |
|---|---|
| macOS / iOS | **VideoToolbox** — implemented here |
| Linux / Windows / Android | **Vulkan Video** (`VK_KHR_video_encode_h265`), one API across AMD, Intel and NVIDIA |
| Windows alternative | D3D12 Video Encode |
| anywhere, fallback | hpvca — the existing software path |

Two dead ends worth recording so they are not re-explored: **Metal exposes no
video encode API at all** (Apple ships that capability only through
VideoToolbox/AVFoundation), and **MoltenVK does not implement Vulkan Video
encode**, so Vulkan is not a way to reach Apple's encoder either. Metal or Vulkan
*compute* would mean writing an HEVC encoder from scratch, which the Amdahl bound
above caps at ~1.9x — far worse than simply calling the ASIC.

**Status: wired.** `--engine portable` builds `MuxEngine<VideoToolboxCodec>` and
falls back to `HpvcaCodec` when the hardware path cannot serve the job.
`--engine hpvca` (alias `software`) forces the software codec.

The fallback decision is made **before** encoding, from the base image and the
requested quality (`Engine::for_job`), not by starting a hardware encode and
recovering from its error. Two reasons: the two codecs produce different files, so
a silent post-hoc substitution would make a benchmark row or a hash comparison a
lie; and the engine *name* then reports the codec that actually ran, which is what
`bench --json` records. Two conditions trigger it today, both stated on stderr:

| condition | why |
|---|---|
| base is not 8-bit | `PF_BGRA` and `PF_L008` are 8-bit; truncating silently would be worse |
| `--quality >= 95` | that asks for 4:4:4 chroma, and the BGRA path is 4:2:0 only |

Two things in the profile *are* worth pursuing, neither of them large:

- `intra::is_available` costs **1.15 s (3.73%)** — more than
  `predict_angular_into` (1.13 s), the function that does the actual prediction.
  A neighbour-availability predicate should be a bitmask test, not the
  second-hottest intra function. Looks like an upstream inefficiency; worth
  reporting to hpvca.
- **9.93% in the kernel across 70 threads** on a 10-core machine. Encoding base
  and gain concurrently gives each its own hpvca thread pool, so the pools
  oversubscribe. Capping `EncodeConfig::threads` per pool might recover part of
  it — worth perhaps 5% of the encode, not more, and untested.
- RDOQ is 22.8% and there is no public knob to disable it: `Speed::Fast` only
  turns off `rdoq_in_loop`, the in-loop shortlist variant. An hpvca option to
  skip RDOQ entirely would be the single biggest remaining lever.

How far quality can buy time, base encode only, 60 MP real photograph, SAO off
(`examples/probe_engine_b_speed`):

| quality | ms | bytes | vs Engine A's 590 ms |
|---|---|---|---|
| 85 | 2965 | 15,634,701 | 5.0x |
| 70 | 2509 | 9,299,339 | 4.3x |
| 50 | 1899 | 3,977,542 | 3.2x |
| 30 | 1634 | 1,529,943 | 2.8x |

At q30 the file is a tenth the size and visibly degraded, and the base encode
*alone* is still 2.8x Engine A's whole encode at q85. **The software codec cannot
reach parity at 60 MP at any quality**; the floor is roughly 3x. Its wins are at
small sizes and in portability, which is what it exists for — and reaching parity
was never its job, since the codec is swappable. That is what the hardware path
above is for.

`bench` derives at `DeriveOptions::default()`, which is **subsample 1** — a
full-resolution gain plane. `convert` defaults to `--gain-subsample 2`, a
quarter of the gain pixels. The two engines here encode identical bytes so the
comparison is sound, but these numbers are not comparable to a `convert` run of
the same file.

The 60 MP row is the interesting one for the sizes real cameras produce. It is a
9504×6336 16-bit TIFF, the shape a Lightroom export has, and it needed a fix
before Engine B would open it at all — `image`'s default 512 MiB allocation cap
rejected 345 MiB of samples plus the decoder's working set.

### What load+derive used to cost

That column was 46.7 / 706.5 ms before the pipeline was profiled against a
61 MP raw, and the conclusion drawn from it — that derivation, not the codec,
was the bottleneck on large images — is no longer true. The shared stages are
now roughly 6x faster and Engine A's *encoder* is the larger cost at every size
above a megapixel. What changed, in descending order of effect: the transfer
functions became exact lookup tables instead of a few hundred million `powf`
calls, and every full-frame pass became row-parallel. See `crates/tohdr-core/src/par.rs`.

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

Engine B cannot be run on this input directly — **its limitation is the decoder,
not the encoder**. Both of its codecs would encode this fine; the pure-Rust
*readers* take TIFF/PNG/JPEG, not HEIC, and say so rather than guessing:

```
error: loading /…/IMG_4913.HEIC: unsupported extension Some("heic")
       (want tif/tiff/png/jpg/jpeg)   [exit 1, no file written]
```

`examples/probe_hw_planes.rs` gets around this by loading through ImageIO and then
handing the identical derived planes to each engine, which is why the 24.5 MP
table above exists at all.

## Caveats

- One machine, one run per configuration. Means over 10 and 5 iterations
  respectively; min/max are in the `tohdr bench --json` output.
- **The media block is shared and its timings are noisier than the CPU codec's.**
  At 0.79 MP the hardware path ranged 34.8–144.0 ms across ten iterations, a 4x
  spread, against Engine A's 79.1–124.2. Compare means, not single runs; earlier
  single-run readings of this comparison landed anywhere from 0.78x to 1.36x.
- Fidelity is compared at matched `--quality`, where the three codecs land within
  1 dB of each other at 12 MP but at very different bitrates (191 / 91 / 76 KB).
  The fidelity-matched comparison is in "The 21 dB bug" above; a *rate*-matched
  one (equal bytes, compare dB) has not been run.
- Fidelity is measured on one 12 MP photograph. The 60 MP fixture cannot
  discriminate — its 5.45 stops of headroom clamp to 2.90 on reconstruction, so all
  three engines score an identical 18.77 dB there.
- The synthetic source is smooth and compresses far better than a photograph.
  Absolute file sizes here are not representative; the *ratio* between engines
  is the transferable part, and even that is scene-dependent.
- `roundtrip` uses `DeriveOptions::default()` (full-resolution gain plane),
  while `tohdr convert` defaults to `--gain-subsample 2`. Consistent across both
  engines, so the comparison holds, but the absolute sizes differ from the CLI's.
- Neither engine has been tested on the iOS WeChat app, the symptom that
  started this project. That remains a device test nobody has run.
