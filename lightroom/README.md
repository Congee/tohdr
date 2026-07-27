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
tools/install-lrplugin.sh
```

which builds the CLI, copies the four `.lua` files and the binary, and stamps
`Info.lua` twice: `VERSION.build` becomes `YYMMDDHHMM` in UTC, and an
`installed-from:` comment records the commit. It prints both, the version spelled
the way Plug-in Manager shows it:

```
version:        0.1.0.2607271253
installed from: be03f8cd (2026-07-27T12:53Z)
```

By hand it is:

```sh
cargo build --release -p tohdr-cli
DEST="$HOME/Library/Application Support/Adobe/Lightroom/Modules/tohdr.lrplugin"
mkdir -p "$DEST"
install -m 644 lightroom/tohdr.lrplugin/*.lua "$DEST/"
install -m 755 target/release/tohdr "$DEST/tohdr"
```

The stamp exists because of point 2 below: nothing inside the plugin can tell you
which copy of the Lua is live, and access times on those files do not move when
Lightroom reads them — an hour went into learning that. Plug-in Manager displays
the version, so afterwards the question is answered by looking at it.

It takes two fields because one cannot do both jobs. `VERSION.build` *accepts* a
string — every LrC 15.3 sample carries `build="202604090947-8f3672ed"`, the string
that also names the SDK bundle they shipped in, and the SDK Guide documents the
field nowhere; this was read off `docs/LrC_15.3_*/Sample Plugins/*/Info.lua`. But
Plug-in Manager does not render a string as the fourth component of `0.1.0.x`:
two installs four hours apart carried different strings and looked identical on
screen. So `build` is a *number*, as Adobe's older samples have it
(`build=200000`), and time-based rather than commit-based so that it also rises
on a reinstall from a dirty tree — which is exactly when "is this my edit?" gets
asked. The commit keeps its precision on the `installed-from` line, and
`-dirty` marks uncommitted changes under `lightroom/` or `crates/`.

Two conventions follow. **`revision` gets bumped on every change that ships**, so
the version reads as a change rather than only as a reinstall; the checkout keeps
`build = 0`, so a stamp cannot be committed by accident.

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
| Copy the camera's MakerNote from the original file | `--maker-note-from <raw>` |
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

### The camera's MakerNote

The intermediate cannot carry one. Measured on `DSC07746.ARW` and the TIFF
Lightroom exports from it: 42 of the raw's 60 standard Exif tags reach the TIFF
and **none** of Sony's 124 `MakerNote` ones. A vendor block is opaque to a
renderer, and it is addressed with offsets into the raw file, so there is nothing
sensible for Lightroom to forward.

So with the box checked the plugin asks the catalog for the original file —
`photo:getRawMetadata('path')`, which the SDK documents as *"the current path to
the photo file if available; otherwise, the last known path"*, hence the existence
check — and passes it to `--maker-note-from`. `tohdr` reads about the first 43 KB
of it, lifts tag `0x927C`, and places the blob at the offset it occupied **in the
raw** (5,222 for this file), padding the block out to reach it. Nothing inside the
blob is rewritten, which is what makes it safe: the vendor's own pointers address
the bytes they were written to address. Verified end to end through a live
Lightroom export — `exiftool -a -Sony:all -s -H -n` reports the same 124 tags with
the same raw values off the ARW and off the output, differing on one line that
exiftool computes from the containing file rather than reads.

That lookup must use **`LrTasks.pcall`, never the built-in.** `getRawMetadata`
yields the task's coroutine, Lua 5.1 cannot yield across a C function, and `pcall`
is one — so a built-in `pcall` there does not protect the call, it breaks it, with
*"Yielding is not allowed within a C or metamethod call"*. A test scans for the
built-in and rejects it.

Two caveats, both of which the dialog states:

- **It needs a Portable engine.** Engine A rebuilds its metadata through
  ImageIO's property model, which has a key for Apple's `MakerNote` and none for
  anyone else's, so a Sony block goes in and 0 tags come out. `tohdr` detects this
  from the engine's own `MetadataSupport` and withdraws the graft rather than
  reporting one the file does not contain — the warning then reaches the advisory
  dialog.
- **It describes the capture, not your edit.** As-shot white balance,
  `DynamicRangeOptimizer`, `CreativeStyle: Standard` — all the camera's, and a
  photo developed in Lightroom no longer matches them. This is what every
  exiftool user copying a `MakerNote` already accepts, and it is genuine
  provenance, but it is not a description of the pixels shipped beside it. One
  tag is a live pointer either way: `HiddenDataOffset` names bytes only the raw
  contains, and exiftool duly resolves it against the new file.

Restricted to `RAW`, `DNG` and `JPG` masters. A `PSD` or `TIFF` master is a scan
or a composite; asking `tohdr` to look in one earns a "has no MakerNote to take"
warning per photo, which is a dialog full of notices about files that were never
going to have one. Virtual copies resolve through `masterPhoto`.

### Naming

Two namespaces, one rule each.

**Names we declare are `snake_case`** — locals, our own functions, our own table
fields. All of them, no exceptions.

**Strings Lightroom reads keep the spelling Lightroom expects**, which for
Adobe's is `camelCase`: the `LR_*` export keys, the fields on the export-service
table (`startDialog`, `processRenderedPhotos`), its methods (`getRawMetadata`,
`waitForRender`).

The `tohdr_*` preset keys belong to that second namespace, and they are frozen.
Lightroom writes them into every saved export preset, so renaming
`tohdr_maxSizeEnabled` does not rename a variable — it orphans a stored setting.
The key silently reverts to its default on load, including a user's custom binary
path, and nothing reports that it happened. They keep the spelling they shipped
with; a new key (`tohdr_makerNote`) joins them in that spelling rather than
splitting one table between two conventions. `share 'tohdr_labelWidth'` isn't
persisted, but it is a string handed to Lightroom in the same namespace, so it
matches its neighbours.

Adobe is not internally consistent here either (`LrView` takes
`fill_horizontal` and `width_in_digits`), so matching it everywhere was never an
option; the boundary above is what can actually be held.

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
- `lua lightroom/tests/test_TohdrCli.lua` — **159 checks, 0 failures**. Covers
  command-line construction and, most importantly, shell quoting: spaces,
  embedded single quotes, `$(...)`, backticks, semicolons, backslashes, double
  quotes and unicode all survive as literals. Also binary-location precedence,
  failure summarising, exit-status decoding (`LrTasks.execute` returns the
  shell's wait status, so exit 1 arrives as 256), success advisories (that a run
  with nothing to say produces no dialog at all, that only the CLI's own
  `tohdr: note:`/`tohdr: warning:` prefix counts so a filename cannot fake one,
  and that the same condition across many photos collapses to one line with a
  count), that the removed PATH-guessing helpers stay removed, and that
  `locate_binary` is unaffected by a sandbox with `os.getenv` deleted.

  It also freezes the eleven `tohdr_*` export-preset keys against a list, and
  checks that every dialog default and every widget binding names one of them.
  That guard exists because it was needed: the snake_case pass renamed all seven
  camelCase keys, which reads as a refactor and is actually silent data loss —
  Lightroom does not find a renamed key in a saved preset, so the setting reverts
  to its default with no error, the user's custom binary path included. Verified
  the guard fails: renaming one key in a scratch copy produces seven failures,
  from the declaration, the frozen list, the default and the binding.

  A third scan covers the two ways Lightroom's Lua is not stock Lua, each of
  which cost a live export to learn: **no built-in `pcall`** (only
  `LrTasks.pcall` — see below), and **nothing from `os`**. Comment-only lines are
  stripped first so the prose explaining the rules is not read as breaking them,
  and each entry keeps its true line number rather than its index in the filtered
  list. Verified both fail on a planted violation, and that the reported line is
  the real one.

  Two more earn their keep by having caught something. `gain_map_source` must
  read the JSON, because the gate that guards this plugin's entire purpose — an
  SDR intermediate yielding a washed-out HEIC reported as a success — searched the
  *prose* for `gain map: derived`, and the plugin passes `--json`, under which the
  CLI emits that line nowhere. The gate could not fire. Its replacement is
  anchored to the start of a line, because the test for a filename containing
  `gain map: derived` failed the first version — and because the opposite message
  ends `…not derived`, so any substring search finds the word in the line that
  means the reverse.

  The count is environment-independent by design — verified identical under
  `PATH` empty, `PATH` long, and `HOME` unset. It used to be 172 only because a
  loop asserted once per `PATH` entry, so it silently tracked the length of the
  tester's own `PATH`; that number was never reproducible on another machine.
- `sh lightroom/tests/test_cli_contract.sh` — **25 checks, 0 failures**. Runs
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
  the real exit code, so `decode_exit_status` unpacks a real wait status correctly
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

- **That a saved export preset survives a plugin update, read back from LrC's
  own preferences.** After the keys were restored, an export wrote
  `sdk_com.tohdr.lightroom-export` in
  `~/Library/Preferences/com.adobe.LightroomClassicCC7.plist` holding all eleven
  `tohdr_*` keys with non-default values intact — `tohdr_engine = "portable"`,
  `tohdr_maxSizeEnabled = true`, `tohdr_maxSizeValue = 4`. That plist is also the
  place to read *what actually ran*, which is how the export below was diagnosed
  without guessing at the dialog state.

- **The advisory dialog renders, and it is what found the bug below.** It read:

  > The conversion succeeded, with notes:
  >
  > \- warning: the camera's MakerNote was requested, but the catalog would not
  > answer (Yielding is not allowed within a C or metamethod call)

  `LrDialogs.message` with a collected advisory in it, on screen, with the CLI's
  wording and the plugin's reason side by side. It was worth building: an export
  that succeeds while quietly dropping what you asked for is the failure mode this
  channel exists to catch, and here it caught one on its first outing.
- **That the built-in `pcall` cannot wrap a catalog call**, which is why that
  first export took no `MakerNote`. `getRawMetadata` yields the task's coroutine —
  that is the real reason the SDK insists on a task to call it from — and Lua 5.1
  cannot yield across a C function, which `pcall` is. So the guard *caused* the
  failure it was meant to contain: every lookup raised *"Yielding is not allowed
  within a C or metamethod call"*, `original_path` returned nil, the flag was never
  passed, and the conversion went ahead without it. `LrTasks.pcall` is Adobe's
  yield-safe form — "simulates Lua's standard `pcall()`, but in a way that allows a
  call to `LrTasks.yield()` to occur inside it" — and a source scan in the test
  suite now rejects the built-in outright.

  Everything measurable had pointed the other way, which is worth recording
  because it is why the reason string was built instead of another guess:

  - the prefs plist records `tohdr_engine = "portable"` and
    `tohdr_makerNote = true`, so Engine B ran with the box checked, and Engine B
    is the engine that *can* carry a foreign blob;
  - the catalog (read `immutable=1`) gives the master as `fileFormat = RAW`,
    `masterImage` null, path `…/7.19/DSC07746.ARW`, a file that exists — so
    `original_path`'s format, virtual-copy, path and existence gates all pass;
  - both files' Exif is little-endian, so `byte-order-differs` cannot fire;
  - `exportRendition.photo` is documented, as are `isVirtualCopy`, `masterPhoto`,
    `fileFormat` and `path` as raw-metadata keys, so no name is wrong;
  - and replaying the *real* Exif through the CLI carries it: feeding the exported
    HEIC back as the source with `--engine portable --maker-note-from` the ARW
    reports `"maker_note_graft":"carried"`, 38,332 bytes pinned at 5,222, and the
    result holds all **124** Sony tags, the same count as the ARW.

  Every one of those was true and the feature still did nothing, because the fault
  was in the wrapper rather than in anything it wrapped. No amount of further
  reading would have found it: it took the plugin reporting the error text
  verbatim. Which is the lesson worth keeping — the fix that mattered was making
  the failure *speak*, and the one-line repair followed from what it said.

  That reporting is now permanent, because the underlying gap was real: `tohdr`
  warns about every `MakerNote` it refuses, but it cannot warn about a companion
  file it was never handed, so a failed lookup was silent and indistinguishable
  from success. `original_path` returns a reason with its nil and the plugin
  raises it as an advisory, worded identically across photos so 200 renditions
  collapse to one line with a count. Same shape as the `gain_map_source` gate
  before it: a check that cannot report is a check that does not exist.

- **The whole chain, in one export.** With `LrTasks.pcall` in place, the same
  photo through the same preset — engine `portable`, box checked, 4 MB budget —
  produced a HEIC carrying **124 Sony tags, byte-for-byte the ARW's**:

  ```
  exiftool -a -Sony:all -s -H -n   ARW: 124 tags   HEIC: 124 tags
  diff: one line
    0x0000 HiddenDataOffset   139264  ->  140212      (+948)
  ```

  That +948 is the Exif block's own offset in the HEIC, which is exiftool
  resolving a stored pointer against the containing file — `HiddenDataOffset`
  names bytes only the raw holds, and it would move like this in any file anyone
  copied a `MakerNote` into. Every other tag, including the ones that describe the
  capture (`DynamicRangeOptimizer`, `CreativeStyle`, as-shot white balance), is
  identical.

  The gain map is untouched by the graft: `tohdr verify` passes every invariant,
  4601x3068 `L008` plane, both flavors, 2.524 declared stops, and the file grew by
  42,910 bytes — the 38,332-byte blob plus the padding that puts it back at offset
  5,222 — to 3,879,046, still inside the 4 MB budget. No advisory dialog, which is
  the correct outcome: a carried graft is a `step` line, not a `note:` or
  `warning:`, so there is nothing to report.

**Still not verified:**

  - the prefs plist records `tohdr_engine = "portable"` and
    `tohdr_makerNote = true`, so Engine B ran with the box checked, and Engine B
    is the engine that *can* carry a foreign blob;
  - the catalog (read `immutable=1`) gives the master as `fileFormat = RAW`,
    `masterImage` null, path `…/7.19/DSC07746.ARW`, a file that exists — so
    `original_path`'s format, virtual-copy, path and existence gates all pass;
  - both files' Exif is little-endian, so `byte-order-differs` cannot fire;
  - `exportRendition.photo` is documented, as are `isVirtualCopy`, `masterPhoto`,
    `fileFormat` and `path` as raw-metadata keys, so no name is wrong;
  - and replaying the *real* Exif through the CLI carries it: feeding the exported
    HEIC back as the source with `--engine portable --maker-note-from` the ARW
    reports `"maker_note_graft":"carried"`, 38,332 bytes pinned at 5,222, and the
    result holds all **124** Sony tags, the same count as the ARW.

  So the CLI half is proven against the real metadata, not a synthetic stand-in —
  the earlier 42-of-60 table was measured on "a 16-bit TIFF with Lightroom-shaped
  metadata injected", which is exactly the kind of proxy this contradicts. Two
  candidates remain: `original_path` returned nil for a reason the SDK's docs do
  not predict (`getRawMetadata` inside `processRenderedPhotos`), or the graft was
  refused with a warning that reached a dialog nobody recorded.

- **That Plug-in Manager renders a numeric `VERSION.build`** as the fourth
  component. That it does *not* render a string one is now known — two installs
  four hours apart carried different strings and looked the same — which is why
  the stamp is a number. `0.1.0.2607271253` parses and `dofile`s, but has not been
  looked at on screen.

The plugin is installed in the `Modules` folder on this machine, so the first two
lists reflect a real Lightroom, not inference. Every item in the second list got
there by someone clicking Export rather than by reading the code.

One caveat on reproducing any of this: **Lua changes need a Lightroom restart**,
and access times are no way to check whether one happened. After a confirmed
restart the installed `.lua` files' atimes had not moved while the binary's had,
so an unchanged atime says nothing about whether Lightroom re-read the file. A
conversion picks up a newly installed `tohdr` immediately; a newly installed
`ExportServiceProvider.lua` does not take effect until Lightroom is restarted.
