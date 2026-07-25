# HDR Gain-Map HEIC — Lightroom Classic export plugin

Exports photos as gain-map HEICs by handing Lightroom's rendered pixels to the
`tohdr` CLI. Same engines, same flavors, same size budget as the command line.

## Requirements

- **Lightroom Classic 13.0 or newer.** Not an API dependency — every Lua call
  here predates it — but 13.0 is where *HDR Output* and HDR editing arrived.
  On an older version the plugin would faithfully build gain maps out of
  already-clipped SDR pixels, which is worse than refusing.
- A `tohdr` binary. Build with `cargo build --release -p tohdr-cli`.

## Installing

1. `File > Plug-in Manager… > Add`, and pick `lightroom/tohdr.lrplugin`.
2. Point the plugin at the binary, by any one of:
   - copying `target/release/tohdr` into `tohdr.lrplugin/` (checked first),
   - putting it on your `PATH`,
   - setting **Custom tohdr path** in the export dialog.

   Lightroom launches from Finder and so inherits a minimal `PATH` — usually
   without Homebrew, Cargo or Nix. `TohdrCli.defaultPathEnv` appends the usual
   prefixes, but bundling the binary or setting the explicit path is the
   reliable option.
3. `File > Export…`, choose **HDR Gain-Map HEIC** as the export-to target.

## Settings

| Setting | CLI equivalent |
|---|---|
| Flavor — Apple / ISO 21496-1 / Both | `--flavor` |
| Engine — Apple ImageIO / portable | `--engine` |
| Maximum file size (e.g. 4 MB) | `--max-size` |
| Quality, minimum quality | `--quality`, `--min-quality` |
| Tone map — clip / Reinhard | `--tone-map` |
| Custom tohdr path | — |

The maximum-size box maps onto the CLI's quality search: it steps quality down
until the file fits, and **fails the photo loudly** rather than writing an
oversized file. Failures surface per photo through `uploadFailed`, carrying the
CLI's own last line of stderr rather than a bare exit code.

The intermediate render is forced to an uncompressed 16-bit **HDR sRGB (Rec.
709)** TIFF with `LR_enableHDRDisplay`, and deleted after conversion. That is
not configurable on purpose: a JPEG or 8-bit intermediate has already discarded
the above-white highlights, and would yield a structurally perfect file
containing no HDR.

What that intermediate actually contains is worth knowing, because it changes
what the CLI does with it. Lightroom does not write one HDR image; it writes a
**pair** in one TIFF — the SDR rendition in `IFD0`, and a full-resolution
3-channel 16-bit gain map in a SubIFD (`PhotometricInterpretation = 52553`)
carrying ISO 21496-1 metadata in tag 52557. So `tohdr` transcodes a finished
gain map and ships *Lightroom's own* SDR rendition as the base, instead of
tone-mapping one itself. See `crates/tohdr-portable/src/gainmap_tiff.rs`.

Because that is the whole point, the plugin verifies it happened: if `tohdr`
reports `gain map: derived`, the intermediate had no gain map in it, the output
is deleted and the photo fails with an explanation. A gain map derived from an
already-clipped SDR rendition describes no HDR at all, and shipping one is the
exact failure this project exists to prevent.

Three settings keys were wrong before LrC's own preferences were dumped and
read: `LR_export_useHDR` does not exist (it was silently ignored, so every
intermediate was SDR), `ProPhotoRGB` is a wide-gamut *SDR* space that the CLI
then misread as PQ with sRGB primaries, and TIFF takes its bit depth from
`LR_export_bitDepthOthers` rather than `LR_export_bitDepth`. None of this is in
the LrC 15.3 SDK Guide, in which the string "HDR" does not appear at all.

## Verification status

Be clear about which half of this is proven.

**Verified here, by running it:**

- All four plugin files and the test file parse: `luajit -bl` on each, 5/5 OK.
- `lua lightroom/tests/test_TohdrCli.lua` — **82 checks, 0 failures**. Covers
  command-line construction and, most importantly, shell quoting: spaces,
  embedded single quotes, `$(...)`, backticks, semicolons, backslashes, double
  quotes and unicode all survive as literals. Also binary-location precedence,
  PATH splitting (including that empty entries are dropped, so a `tohdr` in the
  current directory is never executed silently), and failure summarising.
- `sh lightroom/tests/test_cli_contract.sh` — **19 checks, 0 failures**. Runs
  the real binary and asserts every flag the plugin emits exists, and that every
  flavor/engine/tone-map/size string the dialog can produce is accepted. This is
  the guard against `cli.rs` being renamed out from under the Lua.
- Lightroom Classic 15.4.1 is installed on this machine, so the declared
  `LrSdkVersion = 13.0` is satisfiable here.

**Not verified — nobody has run this inside Lightroom:**

- That the plugin loads in the Plug-in Manager.
- That the export dialog renders, and that the `LrView` layout is right.
- That `processRenderedPhotos`, `waitForRender`, `uploadFailed` and
  `configureProgress` behave as expected against a real export session.
- That `LR_enableHDRDisplay` / `LR_export_colorSpaceNonJPEG = 'sRGB_hdr'` /
  `LR_export_bitDepthOthers` are accepted under those names by an export
  service. The names and values are confirmed to be what the *dialog* stores
  (`AgExport_enableHDRDisplay`, `AgExport_export_colorSpaceNonJPEG`,
  `AgExport_export_bitDepthOthers`), and a hand-driven export with exactly
  those settings does produce a gain-map TIFF — but the `AgExport_` to `LR_`
  mapping is a convention read off the documented keys, not something Adobe
  states for these three. If Lightroom ignores them the plugin now fails the
  photo with an explanation instead of shipping an SDR file.
- Any end-to-end export of an actual photo.

Installing the plugin needs the Plug-in Manager GUI, and this work did not
modify the Lightroom configuration on this machine. Treat everything in the
second list as written-but-untested.
