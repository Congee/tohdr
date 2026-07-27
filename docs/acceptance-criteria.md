# What "as good as IMG_4913" means, measurably

`IMG_4913.HEIC` (iPhone 17 Pro capture) renders as real HDR everywhere it has
been tried, including the iOS WeChat app. `DSC07752.heic` and
`DSC07752_iso.heic` render washed out there. This file turns "same or better
effect" into checks a machine can run, so the claim is never a matter of
squinting at two phones.

Every criterion below is either **[verify]** — checked by at least one automated
checker, exit non-zero on failure — or **[manual]**, with the reason it cannot be
automated.

**`[verify]` does not mean `tohdr verify` alone covers it.** The tag was read
that way for a while and it hid real holes: three criteria were unimplemented in
Rust, one was a tautology that could not fail, and one was enforced by neither
checker while a CLI flag implied otherwise. Which tool owns what:

| Criteria | Covered by |
|---|---|
| 1, 4, 7, 9 | `verify_gainmap.py` only — they need raw box walking, which `ReadBack` has no vocabulary for |
| 2, 3, 5, 6, 8, 10 | both, and they must agree; a disagreement is a bug in one of them |
| 11 | `tohdr verify` only — needs a reference file, which the Python checker has no mechanism for |
| 12, 13 | `cargo test` (`tohdr-core/tests/hdr.rs`); the PSNR half is only ever *measured* by `cargo run --example roundtrip`, never asserted |
| 14 | `tohdr verify` only — it *is* the ImageIO oracle, which the Python checker deliberately avoids being |
| 16 | enforced at write time in `convert.rs`, re-verified by neither |

So a full gate is `tohdr verify` **and** `verify_gainmap.py` **and** `cargo test`.
Any one alone leaves criteria unchecked.

`tools/verify_gainmap.py` checks the container criteria **independently**, sharing
no code with the Rust crates: it walks the boxes from raw bytes with stdlib
`struct` and cross-checks the headroom against `exiftool`. `tohdr verify` uses
our own reader, so a bug present in both our reader and our writer would sail
through it; the Python checker exists to make that class of self-consistent error
visible. Measured discrimination on the three reference files: IMG_4913 passes
11/11 and exits 0; `DSC07752_iso` fails criteria 2, 5, 8, 10 and exits 1;
`DSC07752` fails criterion 8 and exits 1. A conversion **of `IMG_4913.HEIC`** now
passes 11/11 through either engine, because it carries the source's MakerNote and
realigns its headroom tag (§8 below). Converting a source that has no MakerApple
tags of its own — a TIFF or a JPEG — still passes 10/10 applicable and skips 8,
which is the correct outcome rather than a gap: there is no tag to check.

That last case is where having two checkers paid for itself. `tohdr verify`
**failed** it — "Apple aux image present but MakerApple tags are missing" — while
the Python checker skipped it, so the two disagreed about a file that was in fact
correct, and every TIFF or JPEG conversion exited non-zero for obeying §8's
"never from nothing" rule. The Rust checker was the wrong one and now skips too.
Measured on all four files, both checkers agree: `tohdr verify` and
`verify_gainmap.py` exit 0 on `IMG_4913.HEIC` and on a TIFF conversion, and 1 on
both `DSC07752*`. A disagreement between them is by construction a bug in one of
them, and should be chased rather than explained away.

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

   `tohdr verify`'s `gain_plane_present` used to test only that ImageIO
   *reported* a format, never that it equalled `L008`, so it read `[ok]` on
   `DSC07752_iso`'s `420f` plane — the very file this criterion names. The exit
   code was still 1 because criteria 5 and 8 fail there too, which is why the
   hole went unnoticed: a regression breaking only the channel count would have
   passed. Both checkers now fail it.
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

   Precisely: `alt_headroom == max(0, max_log2)`. The floor is forced by the wire
   format, not slack — the headroom fields are **unsigned** while `gain_map_max` is
   signed, so a darkening plane can declare nothing but zero. Setting it unclamped
   wrote 0 while `max_log2` stayed negative, breaking this criterion by the full
   magnitude on round trip.

   Flooring costs no achievable gain: libavif's `alt < base` branch weights the map
   `-clamp((H - base)/(alt - base), 0, 1)`, which is 0 for every display headroom
   `H >= 0`. The encoding that *would* apply a darkening map needs
   `base_headroom > alt_headroom`, making the base the high-headroom rendition and
   contradicting criterion 6. So a darkening map with an SDR base is inexpressible
   here and zero is the honest encoding. Pinned by
   `iso21496_round_trip_holds_criterion_5_for_a_darkening_map`.
6. **[verify]** `base_headroom == 0` for an SDR base.
7. **[verify]** Passes libavif's own validation (`avifGainMapValidateMetadata`,
   `src/gainmap.c:431-448`): all denominators nonzero, `max >= min`, gamma
   numerator nonzero. Note what it does *not* check — channel-count consistency
   — which is why `DSC07752_iso`'s redundant `is_multichannel=1` is survivable
   and not on this list as a defect.
8. **[verify]** MakerApple tag 48 (`HDRGain`) is **non-negative**, and tag 33
   (`HDRHeadroom`) is present when tag 48 is and is itself non-negative — it selects
   which branch of Apple's headroom formula applies (`>= 1.0` vs `< 1.0`), so a
   negative value selects nothing.

   Unreachable through our own writers (`tags_from_headroom` hardcodes tag33 to
   `1.0`, `align_apple_headroom` raises anything below it), so this guards
   third-party and hand-corrupted input. `DSC07752` carries `-0.008120966145`, which
   `chemharuka/toGainMapHDR`'s unclamped `(3.0 - stops) / 70` branch produces for
   any headroom above 8x — reproduced to ~2e-10.

   *Caveat:* that negative value still decodes back to 11.86x through Skia's parser,
   so it is a symptom of the same over-declaration as #5, **not** independently
   proven to cause the washout.

   **When our engines write these tags.** Never from nothing — but a conversion
   of a source that *has* an Apple MakerNote now carries it (see
   `metadata-passthrough.md` §2), which means carrying its tags 33 and 48. That
   is only legitimate if they state *this output's* headroom rather than the
   source's, so `tohdr_portable::align_apple_headroom` rewrites tag 48 in place
   to whatever this conversion derived, and removes both tags when the formula
   cannot express it. The reasoning below is what decides which of those two
   happens; it has not changed, it now has a caller.

   Apple's tag formula tops out at 3.0 stops: in
   the `tag33 >= 1.0` regime it is `stops = -70 · tag48 + 3.0`, so a headroom
   above 8x can only be expressed by pushing tag48 *negative* — precisely how
   `DSC07752.heic` ended up at `-0.00812`.
   `tohdr_core::apple::tags_from_headroom` clamps instead (pinned by
   `clamps_above_8x_instead_of_reproducing_the_washout_bug`), which is the safe
   behavior but means the tags then **understate** the headroom.

   That leaves no good option for a scene needing more than 3 stops:

   - write unclamped tags → negative tag48, failing this criterion, and
     reproducing the exact defect in the broken file;
   - write clamped tags → they disagree with the ISO payload, failing
     criterion 9, which exists because disagreeing copies mean some consumer
     reads the wrong number;
   - write no tags → both criteria *skip*, and nothing can read a wrong value.

   The third is the only one that never lies, so it is what we do when there are
   no tags to begin with — and it is also what `align_apple_headroom` falls back
   to above the ceiling, removing tags 33 and 48 from the carried note in place
   while keeping its other 23. Note `IMG_4913.HEIC` declares 2.287109 stops —
   comfortably under the ceiling — which is why Apple could write all three copies
   in agreement, and why a conversion of it can too. The rule is unchanged: write
   the tags **when and only when** the headroom is at most 3.0 stops; above that,
   silence is correct.
9. **[verify]** Where more than one copy of the headroom is written (ISO
   payload, XMP `HDRGainMapHeadroom`, MakerApple tags), all copies agree within
   1e-3. Apple writes it three times and all three agree — a file whose copies
   disagree is one where some consumer will read the wrong one.

   `verify_gainmap.py` originally compared only the ISO and XMP copies, which
   made this criterion blind to the copy most likely to be stale: the MakerApple
   pair a conversion inherits when it carries a source's MakerNote. It now
   compares all three. The extended check passes on `IMG_4913.HEIC` itself (worst
   delta 9.59e-05) and fails a verbatim carry, which is what forced the tag-48
   rewrite in §8.

## C. Predicted render behavior

Computed from metadata via `tohdr_core::hdr::gain_weight`, not measured on a
display:

10. **[verify]** Every display receives all the gain it can show:
    `delivered == min(display_headroom, max(0, max_log2))`, checked across
    1.0–4.0 stops (at 1.0, 1.5, 2.0, 2.3, 2.98, 4.0 in both checkers).

    `tohdr verify` did not check this until recently. It had a `gain_weight`
    check asserting the weight fell within `[-1, 1]` — which `gain_weight`
    guarantees by construction, since it clamps to `[0, 1]` before an optional
    sign flip, so the check could only fail on NaN and stayed green on a file
    whose delivered gain was off by a full stop. It is now
    `every_display_gets_its_stops` and reports identically to the Python
    checker: on `DSC07752_iso` both say *worst at 2.00-stop display: delivered
    1.099, expected 1.960 (err 0.861)*.

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

    "At the same declared headroom" is a condition, not decoration: comparing
    delivered gain between files that declare different headroom measures the
    two *scenes* rather than the two encoders, so the check skips unless the
    declarations match within 1e-3. `tohdr verify`'s `no_worse_than_reference`
    implements it against `--against` (default `IMG_4913.HEIC`, overridable via
    `TOHDR_REFERENCE`).

    *This was enforced nowhere until it was checked.* `verify.rs` computed the
    verdict before the reference was inspected and then printed the reference's
    checks without folding them in, so `--against` looked like it served this
    criterion while contributing nothing to the exit code; `verify_gainmap.py`
    does not implement it at all, and still doesn't — it has no reference-file
    mechanism. Rust-only coverage is therefore expected here, not a divergence.

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
