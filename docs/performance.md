# Performance

Measured on an M1 Max (8 performance + 2 efficiency cores, 64 GB), release
build, against a 9504×6336 (60.2 MP) Sony ARW of 68 MB — the size a full-frame
camera actually produces, and the size at which every constant in the pipeline
stops being negligible.

Engine-to-engine encode numbers live in [engine-comparison.md](engine-comparison.md).
This file is about the pipeline around them.

## Where it started and where it is

| | Before | After |
|---|---|---|
| One 60 MP raw | 7.35 s | **2.60 s** |
| 51 raws (the folder this was tuned on) | ~6 min 15 s | **63.4 s** |
| Throughput | 8.2 files/min | **48.3 files/min** |
| Peak resident memory, one file | 4.25 GB | **2.47 GB** |

Stage breakdown of the 2.60 s, from `cargo run --release --example profile -p tohdr-apple`:

| Phase | ms | % |
|---|---|---|
| decode (ImageIO → `HdrRgb`) | 2001.6 | 66.2 |
| encode (Engine A) | 590.0 | 19.5 |
| derive gain plane | 258.5 | 8.6 |
| tone map → SDR base | 109.5 | 3.6 |
| `peak_luma` | 62.1 | 2.1 |

Our own code — everything except ImageIO's decode and encode — went from
3243 ms to 408 ms, a factor of 7.9.

## The three things that mattered

### 1. Exact lookup tables, not `powf`

The transfer functions ran per sample: 60.2 MP × 3 channels is 180 million
evaluations per pass, several passes deep. Every source here is an integer —
8-bit sRGB, 16-bit PQ — so a table over *every representable code* is not an
approximation of the curve, it is the curve. 256 entries, or 65536.

`crates/tohdr-core/tests/transfer_luts.rs` checks this exhaustively rather than
by sampling: all 256 codes against the exact formula, the encode direction swept
at 200,000 points and required to stay within one 8-bit code, and the round trip
required to be the identity. The one table that quantises its input (linear →
sRGB) is the only one with a tolerance, and that tolerance is one code.

Byte-identical output before and after, on a real file, in both engines.

### 2. Row-parallel passes, without a work-stealing scheduler

Every expensive loop is a per-pixel map or reduction with identical work at
every pixel, which is exactly the case where work stealing buys nothing over a
static split. `crates/tohdr-core/src/par.rs` is ~60 lines on
`std::thread::scope`: chunk by whole rows, one worker per chunk, join. No
dependency.

The one loop that needed care is the gain accumulation, where several input rows
fold into one output row. Decomposing over *output* rows instead of input rows
makes each gain bucket have exactly one writer, so there is no contention and no
atomics — the decomposition is the synchronisation.

### 3. Banded, concurrent decode

The single largest allocation in the program was a full-frame RGBA f32 render
target — 918 MiB at 60 MP — that existed only to be de-interleaved into `HdrRgb`
and dropped. Drawing in horizontal bands straight into the destination deletes
it.

The reason that is not merely a memory optimisation is what the band timings
show (`examples/probe_band_timing.rs`, 8 bands, one fresh `CGImage`):

```
  band  0   1431.8 ms
  band  1    251.4 ms
  band  2    248.8 ms
  ...
  band  7    253.8 ms
```

The render is two different costs wearing one coat: a ~1150 ms one-off decode
charged to whichever draw happens first, and ~250 ms per band of
area-proportional conversion. Only the second is divisible — and CoreGraphics
does allow several threads to draw one shared, immutable `CGImage` into their own
contexts at once (`examples/probe_band_parallel.rs`):

```
   1 thread(s): decode 1145.9 ms + draw 2019.6 ms = 3165.4 ms
   2 thread(s): decode 1065.7 ms + draw 1079.7 ms = 2145.4 ms
   4 thread(s): decode 1054.1 ms + draw 1049.3 ms = 2103.5 ms
   8 thread(s): decode 1292.1 ms + draw  546.7 ms = 1838.8 ms
  10 thread(s): decode 1168.2 ms + draw  417.1 ms = 1585.3 ms
```

4.8x on the draw stage, 2.0x on the render as a whole. Output is byte-identical
to the single-threaded full-frame path — verified by building with a band budget
large enough to force one band and comparing SHA-256 of the finished HEIC.

## `tohdr batch`

The ~1150 ms serial decode is the part of a conversion that cannot be spread at
all. Nine of ten cores idle through it. Overlapping files fills that hole with
another file's parallel work — which is the whole argument for batching, since a
single conversion already uses every core.

Best of three, eight 60 MP raws:

| jobs | s/file | files/min |
|---|---|---|
| 1 | 2.25 | 26.7 |
| 2 | 1.46 | 41.0 |
| 3 | 1.46 | 41.0 |
| 4 | **1.34** | **44.6** |
| 5 | 1.37 | 43.8 |
| 6 | 1.25 | 47.8 |

Two jobs take most of what there is to take. Everything from two to six then
sits inside the run-to-run spread, which is about 20% on this machine, so the
apparent edge at six is not something to design around — while its memory cost,
roughly 2.5 GB per job, is real and linear. The default is four, and `--jobs`
overrides it.

Note that one job in-process (2.25 s/file) already beats one `convert` process
per file (2.60 s), because ImageIO's framework initialisation is paid once.

## Things that were tried and are worse

**Capping each batch job's workers to its share of the cores.** The obvious
anti-oversubscription move — four jobs on ten cores, so two or three workers
each — is worse across the board: 13.6 s against 10.7 s for eight files at three
jobs. A job sitting inside the serial decode is holding a share it is not using,
and a static cap stops anybody else from taking it. Oversubscription is the
right answer here; the scheduler has more information than we do.

**Reading the banding result off a warm `CGImage`.** The first version of
`probe_banded.rs` reused one `CGImage` across all four band counts and appeared
to show banding making the render 34x faster. It was measuring CoreGraphics'
decoded-bitmap cache. Every band count needs its own freshly-opened image, and
with that fixed the honest answer is that banding is *free* (3183 / 3125 / 3041 /
3019 ms for 1 / 2 / 4 / 16 bands), which is a good result and a different claim.

## What is left

`draw_image`'s ~1150 ms one-off is Apple's RAW demosaic. It is single-threaded,
it is not ours, and short of writing a raw pipeline there is nothing to do about
it inside one file — which is exactly why `batch` exists.

After that the largest item is Engine A's encode at 590 ms, inside
VideoToolbox. The remaining shared stages total 430 ms and are already parallel
and table-driven; fusing tone-map and derive into one pass would save a read of
689 MiB and a read of 345 MiB, worth perhaps 30 ms of the 3021 ms total. Neither
SIMD nor GPU has been needed to get here.
