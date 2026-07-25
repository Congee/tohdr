# tohdr-cli

The `tohdr` command line: produce and inspect HDR gain-map HEICs.

```
tohdr convert <INPUT> --output <FILE> [options]
tohdr inspect <FILE> [--json]
tohdr verify <FILE> [--against <REF>] [--json]
tohdr bench <INPUT> [--iterations <N>] [--engine <apple|portable|hpvca>] [--json]
```

Progress and logging go to stderr; the result (human text or `--json`) goes
to stdout, so `--json` output stays pipeable into `jq` etc.

## `convert`

Encode a gain-map HEIC from an HDR source (a plain HDR file, or an existing
gain-map HEIC to remux).

```sh
# Defaults: both flavors, apple engine, quality 85, reinhard tone-map.
tohdr convert IMG_1234.HEIC --output out.heic

# ISO 21496-1 only (the "ios" spelling also works, as a typo alias).
tohdr convert IMG_1234.HEIC --output out.heic --flavor iso

# Fit under an email attachment limit: searches quality down to
# --min-quality, reports what it picked.
tohdr convert IMG_1234.HEIC --output out.heic --max-size 4MB

# Engine B, hard clip instead of Reinhard roll-off, full-res gain plane.
tohdr convert scene.exr --output out.heic \
  --engine portable --tone-map clip --gain-subsample 1

# Engine B pinned to the pure-Rust codec, e.g. to compare against the hardware
# one or to reproduce a file byte for byte on a machine without a media block.
tohdr convert scene.exr --output out.heic --engine hpvca

# Force a specific declared headroom instead of the auto-derived one.
# (Only do this if you know what you're overriding: see the warning below.)
tohdr convert IMG_1234.HEIC --output out.heic --headroom 2.5

# Machine-readable result for scripting.
tohdr convert IMG_1234.HEIC --output out.heic --max-size 4MB --json
```

Flags:

| Flag | Default | Notes |
|---|---|---|
| `--flavor <apple\|iso\|both>` | `both` | `ios` is accepted as a hidden alias for `iso`. |
| `--engine <apple\|portable\|hpvca>` | `apple` | `apple` = ImageIO. `portable` = our muxer with the fastest plane codec available (the VideoToolbox media block; falls back to `hpvca` for a >8-bit base or `--quality >= 95`, saying so on stderr). `hpvca` (alias `software`) forces the pure-Rust codec, and reads only tif/tiff/png/jpg/jpeg — `portable` hands anything else, camera RAW included, to ImageIO's decoder, since Engine B is a claim about the encoder and container rather than the decoder. The engine name in the output reports the codec that actually ran. |
| `--max-size <SIZE>` | none | `4MB`, `4MiB`, `3.5m`, `1500000`. Fails loudly if even `--min-quality` overshoots, rather than shipping an oversized file. |
| `--quality <1-100>` | `85` | Base (and gain-plane) quality. |
| `--min-quality <1-100>` | `40` | Floor for the `--max-size` search. |
| `--tone-map <clip\|reinhard>` | `reinhard` | How the SDR base is rendered from the HDR source. |
| `--gain-subsample <N>` | `2` | Gain-plane downscale factor; `2` matches Apple's own convention. |
| `--headroom <STOPS>` | auto-derived | Overrides the declared headroom. Warns on stderr if it disagrees with what the plane actually encodes — that mismatch is the defect documented in `docs/heic-gainmap-structure.md`. |
| `--json` | off | Emit a `ConvertReport` (quality actually used, byte count, budget-search attempt count) instead of a one-line summary. |

## `inspect`

What's actually in a file's gain map, both flavors, via Apple ImageIO (the
project's correctness oracle):

```sh
tohdr inspect out.heic
tohdr inspect out.heic --json
```

Reports: dimensions, Apple vs. ISO gain-map presence, the gain plane's
resolution and pixel format, the MakerApple tag33/tag48 values and the
headroom they decode to, and the ISO 21496-1 metadata (including whether
`max_log2 == alt_headroom`).

## `verify`

Checks a file against the correctness invariants that separate
`IMG_4913.HEIC` (renders correctly everywhere tested) from the washed-out
exports this project exists to fix. Exits non-zero on any failed check, so
it's usable as a CI gate or from the Lightroom plugin:

```sh
# Compares against IMG_4913.HEIC by default.
tohdr verify out.heic

# Explicit reference.
tohdr verify out.heic --against reference.heic

tohdr verify out.heic --json
```

Checks: both flavors' presence, gain-plane resolution/format, the
`max_log2 == alt_headroom` invariant, MakerApple tag33/tag48 present and
non-negative (a negative tag48 is the `toGainMapHDR` bug), and the gain
weight a ~2.3-stop phone and a ~2.98-stop Mac XDR display would each apply.

## `bench`

Compares the engines on one input — same base, same gain plane, same metadata,
only the encoder/muxer differs. With no `--engine`, all three run: `apple`,
`portable` (hardware), and `hpvca`:

```sh
tohdr bench source.heic
tohdr bench source.heic --iterations 10 --engine portable --json

# Give every iteration its own VTCompressionSession, as a cold process does.
tohdr bench source.heic --no-session-reuse
```

Reports per engine: iterations that succeeded, mean/min/max wall time, the
**first** iteration on its own, the mean of the **rest**, and output size. An
engine that fails (e.g. because it isn't implemented yet) is reported as a failed
row rather than aborting the whole comparison.

First and rest are separated because on the hardware codec they differ by 3–4x:
the first encode of a given size creates a `VTCompressionSession` and brings the
media block up, and every encode after it reuses both. So `first` is what one
`tohdr convert` costs and the rest is what `tohdr batch` costs per file. Engine A
has a smaller version of the same split, from framework initialisation.

## Exit codes

- `0`: success (or, for `verify`, all checks passed).
- `1`: a runtime error (I/O, an engine reporting failure or "not yet
  implemented", `verify` finding a failed check, `bench` finding every
  engine failed).
- `2`: a command-line usage error (from `clap`).
