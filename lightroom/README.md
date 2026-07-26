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

Two routes. The second is what has actually been exercised on this machine.

**Plug-in Manager.** `File > Plug-in Manager… > Add`, and pick
`lightroom/tohdr.lrplugin`. Loads it from wherever it sits, so the repo checkout
stays the live copy.

**Modules folder.** Copy the bundle in, and Lightroom loads it at launch with no
GUI step:

```sh
cargo build --release -p tohdr-cli
DEST="$HOME/Library/Application Support/Adobe/Lightroom/Modules/tohdr.lrplugin"
mkdir -p "$DEST"
install -m 644 lightroom/tohdr.lrplugin/*.lua "$DEST/"
install -m 755 target/release/tohdr "$DEST/tohdr"
```

Either way:

1. **The binary must be bundled.** A `.lrplugin` is self-contained: `tohdr`
   sits beside the `.lua` files, which is what the `install -m 755` line does.
   That is the only automatic lookup — there is no `PATH` search, and putting
   `tohdr` on your `PATH` will not be noticed.

   Three reasons, each on its own sufficient. Lightroom's Lua sandbox has no
   `os.getenv`, so a plugin cannot read `PATH` to search it — the code that
   tried crashed the first real export. A bundled binary is checked first and
   the install step always provides one, so any fallback would only run in an
   already-broken configuration. And a stale `tohdr` found in a guessed prefix
   would be used silently, converting with a build other than the one you just
   made.

   **Custom tohdr path** in the export dialog still overrides everything,
   because that is your explicit choice rather than our guess. It is the
   convenient thing to point at `target/release/tohdr` while developing, so a
   rebuild takes effect without reinstalling.

   The bundle is genuinely portable, not just tidy: `.cargo/config.toml` passes
   `-Wl,-dead_strip_dylibs`, so the binary links nothing outside
   `/System/Library` and `/usr/lib` — no nix store paths, nothing
   machine-specific.

2. **Restart Lightroom.** The `Modules` folder is scanned only at launch, and an
   already-running Lightroom holds the old copy of every `.lua` file — so a
   reinstall over a running Lightroom changes nothing until it restarts.

3. `File > Export…`. In the **Export To:** dropdown at the top of the dialog,
   choose **HDR Gain-Map HEIC**; its settings appear in a panel titled
   **HDR Gain-Map HEIC (tohdr)**. There is no menu item and no module panel —
   this is an export service provider, so the Export dialog is the only place it
   appears.

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
- `lua lightroom/tests/test_TohdrCli.lua` — **56 checks, 0 failures**. Covers
  command-line construction and, most importantly, shell quoting: spaces,
  embedded single quotes, `$(...)`, backticks, semicolons, backslashes, double
  quotes and unicode all survive as literals. Also binary-location precedence,
  failure summarising, that the removed PATH-guessing helpers stay removed, and
  that `locateBinary` is unaffected by a sandbox with `os.getenv` deleted.

  The count is environment-independent by design — verified identical under
  `PATH` empty, `PATH` long, and `HOME` unset. It used to be 172 only because a
  loop asserted once per `PATH` entry, so it silently tracked the length of the
  tester's own `PATH`; that number was never reproducible on another machine.
- `sh lightroom/tests/test_cli_contract.sh` — **19 checks, 0 failures**. Runs
  the real binary and asserts every flag the plugin emits exists, and that every
  flavor/engine/tone-map/size string the dialog can produce is accepted. This is
  the guard against `cli.rs` being renamed out from under the Lua.
- Lightroom Classic 15.4.1 is installed on this machine, so the declared
  `LrSdkVersion = 13.0` is satisfiable here.

**Established by running it inside Lightroom (15.4.1, Modules-folder install):**

- The plugin loads, appears in the **Export To:** dropdown, and its dialog panel
  renders. Getting there took a fix: `supportsIncrementalPublish = 'only'`,
  copied from Adobe's flickr sample, made the service visible *only* under
  Publish Services — so it loaded correctly and appeared nowhere a user looks.
- An export reaches `processRenderedPhotos` and our own Lua runs.
- **Lightroom's Lua sandbox has no `os.getenv`.** Calling it raises `attempt to
  call field 'getenv' (a nil value)` and Lightroom reports "Unable to Export: an
  internal error has occurred". So the inherited `PATH` is not merely minimal
  from in here, it is unreadable, and no Lr API exposes it.

  That killed the PATH fallback outright rather than prompting a repair: the
  binary is bundled, so the fallback was unreachable in any working
  configuration, and searching a hardcoded guess at `PATH` risked silently
  running a stale `tohdr`. `defaultPathEnv` and `splitPath` are gone, and a test
  asserts they stay gone.

  Nothing in the plugin touches `os` any more — `os.time()` in the temp-log name
  became `LrDate.currentTime()`, since which other `os` members survive the
  sandbox is undocumented, and one demonstrated absence is enough to stop
  guessing.

**Still not verified:**

- Any end-to-end export of an actual photo through to a written HEIC.
- That `waitForRender`, `uploadFailed` and `configureProgress` behave as expected
  across a full export session.
- That `LR_enableHDRDisplay` / `LR_export_colorSpaceNonJPEG = 'sRGB_hdr'` /
  `LR_export_bitDepthOthers` are accepted under those names by an export
  service. The names and values are confirmed to be what the *dialog* stores
  (`AgExport_enableHDRDisplay`, `AgExport_export_colorSpaceNonJPEG`,
  `AgExport_export_bitDepthOthers`), and a hand-driven export with exactly
  those settings does produce a gain-map TIFF — but the `AgExport_` to `LR_`
  mapping is a convention read off the documented keys, not something Adobe
  states for these three. If Lightroom ignores them the plugin now fails the
  photo with an explanation instead of shipping an SDR file.

The plugin is installed in the `Modules` folder on this machine, so the first
two lists reflect a real Lightroom, not inference. Treat the third as
written-but-untested — and note that every item in the second list was found by
someone clicking Export, not by reading the code, which is a fair guide to how
much confidence the third deserves.
