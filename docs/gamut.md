# How much colour rendering into sRGB primaries throws away

`tohdr_apple::load_hdr` renders every source into
`kCGColorSpaceExtendedLinearSRGB` and then clamps each component with `.max(0.0)`
(`read.rs`). That clamp is the point of no return for wide-gamut colour: an
extended-range space *can* represent a colour outside its primaries, as one or
more negative components, so the colour survives the conversion and dies at the
clamp.

Measured by `crates/tohdr-apple/examples/probe_gamut.rs`, which renders each file
into extended-linear sRGB *and* extended-linear Display P3, then cross-checks the
two by matrix. The second render exists to prove the premise: if CoreGraphics
gamut-*mapped* instead of preserving negatives, the 709 render would read as
in-gamut and every number here would be vacuous.

```text
                                 outside    dE>=1     dE>=3    worst
                                 Rec.709   of image  of image     dE
  IMG_4913.HEIC (P3 capture)       0.18%     0.01%     0.00%    2.63
  DSC07746.ARW, ImageIO develop   39.41%     1.79%     0.18%    5.48
  DSC07746, LrC HDR sRGB           0.88%     0.00%     0.00%    0.00
  DSC07746, LrC HDR Display P3    12.33%     5.10%     1.37%    5.35
  DSC07746, LrC HDR Rec.2020      12.42%     5.12%     1.38%    9.94
```

Every cross-check agreed to <2e-4, so CoreGraphics does preserve out-of-gamut
colour through an extended-linear conversion. The clamp, not the colour space, is
what discards it.

**The count and the cost are different questions.** 39% of the ImageIO develop is
technically outside Rec.709 but only just — deepest excursion 8.3% of its own
pixel's peak — so the perceptual cost shows on 0.18% of the frame. The Lightroom
P3 export is out of gamut on a third as many pixels and costs 7.6x more, because
its excursions run to 21.6%. The out-of-gamut count alone would have ranked these
two backwards.

**ImageIO's own raw development is a bad proxy for Lightroom's**, recorded because
it was used as one: it understated the real develop by 3x on visibly-affected area
and 7.6x on obviously-affected area, despite that develop being near-neutral in
colour (`crs:Saturation="0"`, all HSL zero, only `crs:Vibrance="+15"`).

The `sRGB` row is the control and behaves as it must: Lightroom already clipped to
Rec.709, so 0.88% of pixels sit *on* the boundary and the clamp costs them dE
0.00. Nothing to lose means nothing lost.

Two further findings. The obvious-hit pixels are entirely yellow and yellow-green
(83%/17%) — a coherent region of the photograph rather than scattered outliers,
which is what makes 1.37% of a frame worth caring about. And 1.94% of the
Rec.2020 export falls outside P3 as well, with worst dE rising 5.35 -> 9.94, so P3
is not a complete answer either — though sRGB -> P3 recovers far more than
P3 -> Rec.2020 does.

One caveat on all three Lightroom rows: `above diffuse white` is 0.00%, so ImageIO
read only `IFD0` and did **not** apply the gain map in the SubIFD. These measure
the SDR base — the plane whose primaries our `colr` declares and whose pixels we
ship, so it is the right plane for this question, but the HDR highlights' gamut is
unmeasured.
