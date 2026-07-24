# What "as good as IMG_4913" means, measurably

`IMG_4913.HEIC` (iPhone 17 Pro capture) renders as real HDR everywhere it has
been tried, including the iOS WeChat app. `DSC07752.heic` and
`DSC07752_iso.heic` render washed out there. This file turns "same or better
effect" into checks a machine can run, so the claim is never a matter of
squinting at two phones.

Every criterion below is either **[verify]** — checked by `tohdr verify`, exit
non-zero on failure — or **[manual]**, with the reason it cannot be automated.

`tools/verify_gainmap.py` checks the container criteria **independently**, sharing
no code with the Rust crates: it walks the boxes from raw bytes with stdlib
`struct` and cross-checks the headroom against `exiftool`. `tohdr verify` uses
our own reader, so a bug present in both our reader and our writer would sail
through it; the Python checker exists to make that class of self-consistent error
visible. Measured discrimination on the three reference files: IMG_4913 passes
11/11 and exits 0; `DSC07752_iso` fails criteria 2, 5, 8, 10 and exits 1;
`DSC07752` fails criterion 8 and exits 1. Engine B's own output
(`--engine portable --flavor both`, synthetic 2.98-stop source) passes 9/9
applicable and exits 0, skipping 8 and 9 — it writes no MakerApple tags and no
XMP headroom, which is the remaining gap against IMG_4913's 11/11.

Reference values come from `docs/heic-gainmap-structure.md`, decoded byte by byte
from the real files; the raw payloads are committed under `assets/fixtures/`.

## A. Structural signaling

The three files fail differently here, which is why all of it is checked
rather than just the parts one broken file got wrong:

| | IMG_4913 (good) | DSC07752 | DSC07752_iso |
|---|---|---|---|
| `auxC` boxes | 6 | 2 | **0** |
| Apple gain-map URN | present | present | **absent** |
| `tmap` item | present | **absent** | present |
| MakerApple tag count | 25 | 2 | 2 |
| tag 48 `HDRGain` | `+0.0525` | **`-0.00812`** | **`-0.00812`** |
| XMP `HDRGainMapHeadroom` | `4.880772` | `11.863581` | **absent** |
| gain-map plane | half-res, 1 ch | full-res, 1 ch | full-res, **3 ch** |
| gamma | `0.825684` | n/a (no ISO) | **`1.0`** |

**The two broken files fail differently, and neither failure mode alone
explains both.** `DSC07752` is a structurally valid *Apple* gain map — correct
URN, correct `auxl`, single-channel plane — whose only measurable defect is the
out-of-domain negative tag 48. `DSC07752_iso` is a valid *ISO* file whose defect
is the 1.61-stop over-declaration (criterion 5), and which additionally lost the
Apple URN, went full-res 3-channel, and reverted gamma to the UltraHDR nominal
`1.0`. What they share is that both **mis-state the headroom**, via different
fields. Hence criteria A and B are both enforced rather than picking a favorite.

On that negative tag 48, precisely: Skia's arithmetic does invert it back to
11.86x, so it is not self-inconsistent. The real problem is that 11.86x is
3.568 stops, above the 3.0-stop ceiling Apple's tag formula can express at all —
which is *why* it went negative. A decoder that clamps tag 48 at zero reads 8x
instead, and one that validates its domain may reject it outright. Out-of-domain
is a defect even when one implementation's algebra survives it.

1. **[verify]** The base image is the primary item (`pitm`).
2. **[verify]** The gain map is a separate image item, single-channel 8-bit
   (`L008`), not a 3-channel RGB image. `DSC07752_iso` ships full-res 3-channel,
   which costs bytes for no fidelity gain on a luma-derived map.
3. **[verify]** With a flavor including Apple: the gain-map item carries an
   `auxC` with URN `urn:com:apple:photo:2020:aux:hdrgainmap`, **and** an `auxl`
   `iref` from the gain map to the base. The URN alone is not enough — a
   consumer needs the back-reference to know which image the map applies to.
4. **[verify]** With a flavor including ISO: a `tmap` derived item exists, its
   `dimg` lists `[base, gain]` in that order, `tmap` appears in `ftyp`'s
   compatible brands, and its payload is exactly 62 bytes (1 `ToneMapImage`
   version byte + the 61-byte single-channel C.2.2 struct).

## B. Metadata correctness

This is where both broken exports actually fail, and the failure is shared:

5. **[verify]** `max_log2 == alt_headroom` (±1e-3). **The single most important
   check.** The gain plane can deliver at most `max_log2` stops; `alt_headroom`
   declares how much the scene needs. IMG_4913 keeps them identical (2.287109).
   `DSC07752_iso` declares 3.568470 while encoding 1.96 — a 1.61-stop
   over-declaration. A conformant renderer weights the map by
   `(display - base) / (alt - base)` (libavif `src/gainmap.c:52-63`), so
   over-declaring makes it *under-apply* the map and the flat SDR base shows
   through. Enforced in code by `tohdr_core::hdr::derive_consistent`.
6. **[verify]** `base_headroom == 0` for an SDR base.
7. **[verify]** Passes libavif's own validation (`avifGainMapValidateMetadata`,
   `src/gainmap.c:431-448`): all denominators nonzero, `max >= min`, gamma
   numerator nonzero. Note what it does *not* check — channel-count consistency
   — which is why `DSC07752_iso`'s redundant `is_multichannel=1` is survivable
   and not on this list as a defect.
8. **[verify]** MakerApple tag 48 (`HDRGain`) is **non-negative**, and tag 33
   (`HDRHeadroom`) is present when tag 48 is. `DSC07752` carries
   `-0.008120966145`, which `chemharuka/toGainMapHDR`'s unclamped
   `(3.0 - stops) / 70` branch produces for any headroom above 8x — reproduced
   to ~2e-10. `tohdr_core::apple::tags_from_headroom` clamps instead.
   *Caveat, stated because it matters:* that negative value still decodes back
   to 11.86x through Skia's parser, so it is a symptom of the same
   over-declaration as #5, **not** independently proven to cause the washout.
9. **[verify]** Where more than one copy of the headroom is written (ISO
   payload, XMP `HDRGainMapHeadroom`, MakerApple tags), all copies agree within
   1e-3. Apple writes it three times and all three agree — a file whose copies
   disagree is one where some consumer will read the wrong one.

## C. Predicted render behavior

Computed from metadata via `tohdr_core::hdr::gain_weight`, not measured on a
display:

10. **[verify]** Every display receives all the gain it can show:
    `delivered == min(display_headroom, alt_headroom)`, checked across
    1.0–4.0 stops.

    *This criterion was originally written as "a ~2.3-stop display applies
    weight 1.0", which was wrong.* That only holds when the scene needs
    ≤ 2.3 stops, so it failed a correctly-built file of a brighter scene — our
    own 2.98-stop test render tripped it while delivering exactly the 2.300
    stops a 2.3-stop phone can display. The invariant above is the one that
    actually distinguishes correct from broken, and it follows from #5 given
    `base_headroom == 0`:
    `delivered = max_log2 · clamp(display/alt) = alt · min(1, display/alt)`.
    It still catches the real defect — `DSC07752_iso` encodes 1.96 stops but
    declares 3.568, so a 2.3-stop display gets 1.263 stops where it should get
    1.96, and the criterion fails. Measured: IMG_4913 passes, `DSC07752_iso`
    fails.
11. **[verify]** No display in `1.0..=4.0` stops receives *less* gain from our
    output than from IMG_4913 at the same declared headroom.

## D. Reconstruction fidelity

12. **[verify]** Round-tripping the source HDR through tone map → derive →
    encode → decode → apply keeps worst-case relative luma error under 6% on
    pixels above 0.05 linear, and PSNR ≥ 40 dB. Dark pixels are excluded because
    8-bit base quantization, not the gain map, dominates the ratio there.
13. **[verify]** Reconstruction actually exceeds linear 1.0. A pipeline that
    clamps at SDR white passes every structural check above while producing no
    HDR at all — this is the check that catches it.

## E. Platform acceptance

14. **[verify]** macOS ImageIO — the platform that decides what Apple apps
    show — reports the gain map on our output. `tohdr_apple::inspect_bytes` is
    the oracle: `apple_aux` and/or `iso_aux` true per flavor, `gain_size` as
    expected, `gain_pixel_format` `L008`. This is deliberately the *platform's*
    opinion and not our own parser's, so a bug shared between our reader and
    writer cannot hide.
15. **[manual]** Renders as HDR in the iOS WeChat app. **Not automatable and not
    yet confirmed.** Everything above is necessary; only a device test is
    sufficient. WeChat's renderer is closed, and the two broken files differ in
    *both* signaling and metadata, so which one WeChat trips on is unproven —
    A and B are both enforced rather than betting on one.

## F. Output constraints

16. **[verify]** `--max-size N` produces a file of at most N bytes, or fails
    loudly. Never silently ships an oversized file.

## Where "better than IMG_4913" is available

Not required to pass, but reachable and worth measuring:

- **Full-resolution gain map.** Apple ships half-res. Full res costs bytes and
  raises fidelity; `--gain-subsample 1`.
- **Three-channel gain map.** ISO 21496-1 permits it. A highlight clipped in one
  channel only (a saturated red light) is under-corrected by a luma-derived map;
  3 channels track it at 3x the metadata cost. Our serializer already handles
  141-byte payloads.
- **10-bit base.** Removes the 8-bit banding that currently sets the noise floor
  for criterion 12 in shadows.
- **Both flavors at once.** IMG_4913 does this; `DSC07752*` each pick one and
  miss the other. `Flavor::Both` is the default for exactly this reason.
