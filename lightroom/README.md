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
| *(not exposed)* base colour space, fixed at P3 | `--colour-space p3` |
| Custom tohdr path | — |

The maximum-size box maps onto the CLI's quality search: it steps quality down
until the file fits, and **fails the photo loudly** rather than writing an
oversized file. Failures surface per photo through `uploadFailed`, carrying the
CLI's own last line of stderr rather than a bare exit code.

The intermediate render is forced to an uncompressed 16-bit **HDR Display P3**
TIFF with `LR_enableHDRDisplay`, and deleted after conversion. That is not
configurable on purpose: a JPEG or 8-bit intermediate has already discarded the
above-white highlights, and would yield a structurally perfect file containing no
HDR.

P3 rather than sRGB because the narrow request is not the neutral one. Rendering
into Rec.709 discards every colour outside it — measured on a real export of
DSC07746, 12.33% of the frame, worst error dE 5.35, concentrated in a coherent
yellow/yellow-green region rather than scattered
(`tohdr-apple/examples/probe_gamut.rs`). The sRGB export's own row in that table
reads dE 0.00 for the least reassuring reason available: Lightroom had already
clipped it, so there was nothing left to lose. For an iPhone capture P3 is
*exactly* enough — 0 pixels of IMG_4913 fall outside it, against 44,799 outside
Rec.709.

This was sRGB until `tohdr` could read the intermediate's embedded ICC profile.
Asking for wider primaries before that would have mislabelled rather than carried
them: the pixels would have been P3 and the output would have said Rec.709, which
no consumer can detect — it simply renders desaturated. `tohdr` now reads IFD0's
profile and declares what it finds, so a Lightroom that ignored the request would
produce a reported mismatch instead of a quietly wrong file.

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
- `lua lightroom/tests/test_TohdrCli.lua` — **87 checks, 0 failures**. Covers
  command-line construction and, most importantly, shell quoting: spaces,
  embedded single quotes, `$(...)`, backticks, semicolons, backslashes, double
  quotes and unicode all survive as literals. Also binary-location precedence,
  failure summarising, exit-status decoding (`LrTasks.execute` returns the
  shell's wait status, so exit 1 arrives as 256), success advisories (that a run
  with nothing to say produces no dialog at all, that only the CLI's own
  `tohdr: note:`/`tohdr: warning:` prefix counts so a filename cannot fake one,
  and that the same condition across many photos collapses to one line with a
  count), that the removed PATH-guessing helpers stay removed, and that
  `locateBinary` is unaffected by a sandbox with `os.getenv` deleted.

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
- **A full export of a real photo, through to a HEIC that passes `tohdr
  verify`.** `DSC07746.ARW` (60.2 MP) exported to a 3.8 MB gain-map HEIC:
  9202x6135 base, 4601x3068 gain plane, both flavors present, 2.524 declared
  stops, every invariant `ok`. So `waitForRender` and `configureProgress` behave
  as assumed across a session — and `uploadFailed` does too, from the export
  before it, which reported `tohdr failed (exit 1)` in the Export Results dialog:
  the real exit code, so `decodeExitStatus` unpacks a real wait status correctly
  in a real Lightroom, not just in the test.
- **That `LR_enableHDRDisplay` and `LR_export_bitDepthOthers` are accepted under
  those names by an export service**, which was previously inferred from the
  `AgExport_` spellings the dialog stores. The intermediate arrived with a gain
  map in it: had it not, the plugin would have deleted the output and failed the
  photo with "Lightroom rendered an SDR intermediate", and instead the export
  succeeded with 2.524 stops of real headroom. A 16-bit HDR TIFF is what
  Lightroom actually wrote.
- **That Lightroom honours `LR_export_colorSpaceNonJPEG = 'p3_hdr'`.** The token
  is LrC's own — it is what LrC writes into `EditInPs_hdr_colorSpace` for its
  Edit-in-Photoshop HDR path, and the export keys take the same `<space>_hdr`
  spellings — and Adobe documents none of these values, so this was read off the
  intermediate rather than trusted.

  The delivered HEIC narrows it without settling it: a base labelled
  `SMPTE EG 432-1` is what you get *either* from a recognised Display P3 source
  *or* from an unrecognised wide one falling back to `--colour-space p3`. It does
  rule out an sRGB fallback, since `primaries_from_icc` is verified to recognise
  the real 3144-byte `sRGB IEC61966-2.1` profile Lightroom embeds (see
  `cargo run --example probe_icc -p tohdr-core`) — had Lightroom ignored the token
  and fallen back, the base would have been labelled `srgb` from that profile.

  So the intermediate itself was read. The plugin deletes it, but not until after
  the conversion, which leaves a window of seconds: a poller on the export
  destination caught `DSC07746.tif` at 677,497,058 bytes and pulled tag 34675
  straight out of IFD0. It is **Apple's Display P3** — 548 bytes, profile version
  4.0.0, creator "Apple Computer Inc.", "Copyright Apple Inc., 2015", colorants
  `0.51512 / 0.29198 / 0.15710` — which `primaries_from_icc` classifies `p3`.
  Lightroom honoured the token, and the output's P3 label came from the source
  rather than from the flag's default. The export also produced no advisory
  dialog, which is the same answer arrived at independently.

  Worth knowing for the recognition code: this is *not* the 536-byte
  `/System/Library/ColorSync/Profiles/Display P3.icc`. Lightroom embeds a
  12-byte-larger variant with the same colorants — which is exactly why
  recognition matches colorants against Bradford-adapted references instead of
  comparing profiles byte for byte.

**Still not verified:**

- That the advisory dialog *renders*. Every export so far has had nothing to
  report, which is the correct behaviour and is why the P3 result is trustworthy —
  but it means `LrDialogs.message` with a collected advisory in it has been
  exercised only in Lua, never on screen. The dialog is the last thing standing
  between a future silent fallback and the user, so it is worth confirming the
  first time an export legitimately trips one.

The plugin is installed in the `Modules` folder on this machine, so the first two
lists reflect a real Lightroom, not inference. Every item in the second list got
there by someone clicking Export rather than by reading the code.

One caveat on reproducing any of this: **Lua changes need a Lightroom restart**,
and access times are no way to check whether one happened. After a confirmed
restart the installed `.lua` files' atimes had not moved while the binary's had,
so an unchanged atime says nothing about whether Lightroom re-read the file. A
conversion picks up a newly installed `tohdr` immediately; a newly installed
`ExportServiceProvider.lua` does not take effect until Lightroom is restarted.
