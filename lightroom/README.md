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

The intermediate render is forced to uncompressed 16-bit ProPhoto RGB TIFF with
`LR_export_useHDR`, and deleted after conversion. That is not configurable on
purpose: a JPEG or 8-bit intermediate has already discarded the above-white
highlights the gain map is derived from, and would yield a structurally perfect
file containing no HDR.

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
- That `LR_export_useHDR` is the correct settings key for HDR output on
  15.4.1, and that Lightroom honours the forced TIFF settings.
- Any end-to-end export of an actual photo.

Installing the plugin needs the Plug-in Manager GUI, and this work did not
modify the Lightroom configuration on this machine. Treat everything in the
second list as written-but-untested.
