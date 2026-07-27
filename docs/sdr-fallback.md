# What an SDR-only device shows

A gain-map HEIC's base image *is* the SDR rendition, and a decoder that knows
nothing about gain maps shows it unmodified. So "will it look washed out on an
SDR device" is not a question about the gain map — it is a question about the
tone map that produced the base, and it needs no second device to answer:
`load_sdr` asks ImageIO for the base rendition, the same pixels an SDR viewer
gets.

Measured by `crates/tohdr-apple/examples/probe_sdr_fallback.rs`. Metrics are on
luma in display (sRGB-encoded) units, the axis the eye judges flatness on:

- `mean`, `p5`, `p50`, `p95` — where the tones sit.
- `spread` = p95 - p5, `rms` = standard deviation. Both fall when an image goes
  flat; "washed out" is exactly this pair dropping.
- `sat` / `sat32` — mean `(max - min) / max`, over non-black pixels and over
  pixels at or above 32. Falls when highlights are pushed toward white.
- `clip%` — pixels with any channel at 255. What `--tone-map clip` spends.
- `dmean` / `dmax` — per-pixel luma difference against the first file, 0..255.

`magick` rows decode the same file through libheif instead of ImageIO
(`magick x.heic -depth 8 PNG24:x.png`), which is the class of decoder an
SDR-only viewer actually has.

## From a plain HDR source, where we render the base ourselves

The iPhone 17 Pro HDR reference capture, 5712x4284 (24.5 MP, gain plane 2856x2142
— exactly half). Reference = Apple's own base, ours via
`tohdr convert --engine apple`:

| base | mean | p95 | spread | rms | sat32 | clip% | dmean | dmax |
|---|---|---|---|---|---|---|---|---|
| Apple's base, ImageIO | 98.6 | 197 | 187 | 64.8 | 0.264 | 1.93% | - | - |
| Apple's base, libheif | 98.6 | 197 | 187 | 64.8 | 0.263 | 2.33% | 0.15 | 7.60 |
| ours `reinhard`, ImageIO | 100.8 | 201 | 190 | 64.4 | 0.260 | 2.65% | 3.15 | 44.45 |
| ours `reinhard`, libheif | 100.8 | 201 | 190 | 64.4 | 0.260 | 3.92% | 3.12 | 44.45 |
| ours `clip`, ImageIO | 113.5 | 239 | 228 | 76.5 | 0.246 | 8.45% | 14.89 | 77.21 |
| ours `clip`, libheif | 113.5 | 239 | 228 | 76.6 | 0.245 | 9.33% | 14.85 | 76.71 |

The default `reinhard` lands 2.2 levels brighter than Apple's own base with 3
more levels of spread, mean difference 3.15/255 — the same photograph. `clip` is
the one that diverges: 15 levels brighter, 41 more spread, 8.45% of pixels
clipped against Apple's 1.93%, because everything above SDR white lands on the
ceiling instead of rolling off.

**`dmean` and `dmax` alone understate that 3.15.** It is not noise. Decoding
both through libheif and differencing per pixel (24,470,208 px): p50 2.57, p90
5.55, p99 10.37, p99.9 21.93. Bucketed by *reference* luma decile it is plainly
structured:

```text
decile 0 (luma   0-13)  mean 1.10     decile 5 (luma 113-130) mean 3.95
decile 1 (luma  13-20)  mean 1.41     decile 6 (luma 130-140) mean 2.73
decile 2 (luma  20-40)  mean 2.04     decile 7 (luma 140-151) mean 1.72
decile 3 (luma  40-80)  mean 3.78     decile 8 (luma 151-187) mean 2.63
decile 4 (luma  80-113) mean 4.68     decile 9 (luma 187-255) mean 5.72
```

It is also spatially clustered: of the pixels above p99.9, **92.1%** have at
least one above-threshold 4-neighbour, against **0.4%** expected at that density
for independent per-pixel noise. Those pixels sit at reference luma 242 on
average (p5 226), and the 15 worst are all at 242-255, against white.

The shape is *two humps*, not a monotonic climb into the highlights: decile 4's
4.68 nearly matches decile 9's 5.72, with a trough at decile 7. That is a tone
curve differing across its whole length, not only in the rolloff — consistent
with Reinhard vs whatever Apple applies, and not something a "3.15/255 mean"
conveys.

It does not change the answer: 0.1% of pixels, concentrated against white, in
the direction of *less* clipping than Apple. But "the same photograph" rests on
the distribution, not the mean.

## From a Lightroom HDR export, where Lightroom rendered the base

`a_srgb.tiff` (LrC "HDR sRGB", 9202x6135), reference = its own IFD0:

| base | mean | p95 | spread | rms | sat | sat32 | dmean | dmax |
|---|---|---|---|---|---|---|---|---|
| Lightroom's SDR rendition | 40.1 | 123 | 120 | 36.8 | 0.609 | 0.605 | - | - |
| ours, ImageIO | 40.1 | 122 | 118 | 36.6 | 0.693 | 0.622 | 1.59 | 29.79 |
| ours, libheif | 40.0 | 122 | 119 | 36.6 | 0.677 | 0.615 | 1.61 | 31.21 |

The tone map never runs here: the source carries its own gain map, so the
pipeline transcodes it and keeps Lightroom's SDR rendition verbatim.
`--tone-map clip` produces a **byte-identical** file, which is the proof.
1.59/255 is HEVC quality-85 noise.

The `sat` jump, 0.609 -> 0.693, is the metric and not the image: it shrinks to
0.017 under `sat32`, because this frame's median is 30 and `(hi-lo)/hi` is
meaningless that close to black. Neither `mean` nor `spread` moves.

## What this settles

An SDR-only device shows the base, and the base is a real SDR photograph on both
paths. Nothing washes out. Two independent decoders agree to 0.15/255 on Apple's
file and 1.6/255 on ours.

## Colour declarations on the base, which are worse than the pixels

Walking `iprp`/`ipco`/`ipma` and reporting `colr` per item (`item 46` is `pitm`
in both files):

| | our output | reference |
|---|---|---|
| base, item 46 | `colr/nclx`, primaries *Unspecified*, transfer *Unspecified*, matrix BT.601, full range | `colr/prof`, 536 B, `"Display P3"` — and **no `nclx` at all** |
| `tmap` item | 66: `colr/nclx`, BT.2020 primaries, PQ transfer, matrix 9 | 122: `colr/prof`, 26,664 B, `"Display P3 Primaries; PQ (Adaptive Gain Curve …)"` |
| items carrying any ICC | **0** | **100** (every base tile, the base grid, the alt tiles, the `tmap`) |

Apple's base has no `nclx` box at all, only the ICC, so the two files declare
their base colour space by entirely different mechanisms — not one merely adding
a profile on top. And the gap is not one profile against zero: the reference capture
attaches an ICC to a hundred items.

Conversely we are less silent than "no ICC at all" suggests: our `tmap` declares
BT.2020/PQ via `nclx`. It is the *base* that says nothing, and the base is what
an SDR-only decoder reads. The guess both measured decoders make (sRGB) is right
today, but it is still a declaration the base should be making and isn't.

The 26,664-byte `tmap` profile is committed as
`assets/fixtures/img4913_tmap_icc_profile.icc` — byte count matches.
