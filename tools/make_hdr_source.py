#!/usr/bin/env python3
"""Generate a deterministic HDR test image as an uncompressed TIFF.

A fair engine benchmark needs the same input bytes on both sides, and an
end-to-end gain-map test needs a source whose true above-white content is known
exactly rather than inferred from a photograph. Stdlib only -- no imaging
dependency to pin.

Two sample formats, because the two engines reach for different decoders:
  --format f32   32-bit IEEE float, linear, 1.0 == SDR diffuse white. Values
                 above 1.0 are the HDR headroom. Unambiguous; the reference.
  --format u16   16-bit unsigned, linear, scaled so `--u16-white` maps to
                 65535. Lossy above white and needs the scale communicated out
                 of band, which is exactly the ambiguity an unmanaged 16-bit
                 TIFF has in the wild -- included so that path is testable.

Usage:
    tools/make_hdr_source.py out.tiff [--width 1024] [--height 768]
                             [--peak 8.0] [--format f32|u16] [--u16-white 1.0]
"""
import argparse
import math
import struct
import sys

# TIFF tags we emit.
IMAGE_WIDTH = 256
IMAGE_LENGTH = 257
BITS_PER_SAMPLE = 258
COMPRESSION = 259
PHOTOMETRIC = 262
STRIP_OFFSETS = 273
SAMPLES_PER_PIXEL = 277
ROWS_PER_STRIP = 278
STRIP_BYTE_COUNTS = 279
PLANAR_CONFIG = 284
SAMPLE_FORMAT = 339

TYPE_SHORT = 3
TYPE_LONG = 4


def scene(width, height, peak):
    """Deterministic linear-light RGB in a flat list, 1.0 == SDR white.

    Content is chosen to exercise the things a gain map is bad at:
      - a broad SDR gradient (the part that must survive tone mapping intact)
      - specular discs far above white (the headroom the map has to carry)
      - a saturated red highlight clipping ONE channel, which a luma-derived
        single-channel gain map necessarily under-corrects
      - near-black patches, where the base/alt offsets decide the ratio
    """
    px = [0.0] * (width * height * 3)
    cx, cy = width / 2.0, height / 2.0

    for y in range(height):
        for x in range(width):
            u = (x + 0.5) / width
            v = (y + 0.5) / height

            # SDR base gradient: a gentle diagonal, comfortably below white.
            r = 0.15 + 0.45 * u
            g = 0.18 + 0.40 * v
            b = 0.22 + 0.30 * (1.0 - u)

            # Near-black corner patch.
            if u < 0.12 and v < 0.12:
                r = g = b = 0.0015

            # Three specular discs at increasing intensity.
            for i, (dx, dy, rad, mult) in enumerate((
                (0.25, 0.70, 0.070, 0.30),
                (0.50, 0.70, 0.055, 0.65),
                (0.75, 0.70, 0.040, 1.00),
            )):
                d = math.hypot(u - dx, v - dy) / rad
                if d < 1.0:
                    # Smooth falloff so the map has a gradient to encode, not a
                    # step edge that only tests clamping.
                    f = math.cos(d * math.pi / 2.0) ** 2
                    lift = 1.0 + (peak - 1.0) * mult * f
                    r *= lift
                    g *= lift
                    b *= lift

            # Saturated red highlight: red far above white, green/blue low.
            d = math.hypot(u - 0.5, v - 0.25) / 0.09
            if d < 1.0:
                f = math.cos(d * math.pi / 2.0) ** 2
                r = max(r, 0.2 + (peak * 0.9) * f)
                g = min(g, 0.10)
                b = min(b, 0.06)

            i = (y * width + x) * 3
            px[i], px[i + 1], px[i + 2] = r, g, b

    # Pin the exact peak so tests can assert on it.
    px[0] = peak
    px[1] = peak
    px[2] = peak
    return px


def write_tiff(path, width, height, samples, sample_format, u16_white):
    """Uncompressed, single-strip, contiguous RGB TIFF (little-endian)."""
    if sample_format == 'f32':
        bits, sfmt = 32, 3  # IEEE float
        body = struct.pack(f'<{len(samples)}f', *samples)
    else:
        bits, sfmt = 16, 1  # unsigned integer
        scale = 65535.0 / max(u16_white, 1e-6)
        clipped = [min(65535, max(0, int(round(v * scale)))) for v in samples]
        body = struct.pack(f'<{len(clipped)}H', *clipped)

    entries = [
        (IMAGE_WIDTH, TYPE_LONG, 1, width),
        (IMAGE_LENGTH, TYPE_LONG, 1, height),
        (BITS_PER_SAMPLE, TYPE_SHORT, 3, None),      # -> out-of-line
        (COMPRESSION, TYPE_SHORT, 1, 1),             # none
        (PHOTOMETRIC, TYPE_SHORT, 1, 2),             # RGB
        (STRIP_OFFSETS, TYPE_LONG, 1, None),         # patched below
        (SAMPLES_PER_PIXEL, TYPE_SHORT, 1, 3),
        (ROWS_PER_STRIP, TYPE_LONG, 1, height),
        (STRIP_BYTE_COUNTS, TYPE_LONG, 1, len(body)),
        (PLANAR_CONFIG, TYPE_SHORT, 1, 1),           # chunky
        (SAMPLE_FORMAT, TYPE_SHORT, 3, None),        # -> out-of-line
    ]
    entries.sort(key=lambda e: e[0])  # TIFF requires ascending tag order

    header_len = 8
    ifd_len = 2 + 12 * len(entries) + 4
    # Out-of-line values: BitsPerSample[3] and SampleFormat[3], 6 bytes each.
    extra_off = header_len + ifd_len
    bps_off = extra_off
    sfmt_off = extra_off + 6
    strip_off = extra_off + 12

    out = bytearray()
    out += struct.pack('<2sHI', b'II', 42, header_len)
    out += struct.pack('<H', len(entries))
    for tag, typ, count, value in entries:
        if tag == BITS_PER_SAMPLE:
            payload = struct.pack('<I', bps_off)
        elif tag == SAMPLE_FORMAT:
            payload = struct.pack('<I', sfmt_off)
        elif tag == STRIP_OFFSETS:
            payload = struct.pack('<I', strip_off)
        elif typ == TYPE_SHORT and count == 1:
            # A SHORT value sits in the low 2 bytes of the 4-byte field.
            payload = struct.pack('<HH', value, 0)
        else:
            payload = struct.pack('<I', value)
        out += struct.pack('<HHI', tag, typ, count) + payload
    out += struct.pack('<I', 0)  # no next IFD
    assert len(out) == extra_off, (len(out), extra_off)
    out += struct.pack('<3H', bits, bits, bits)
    out += struct.pack('<3H', sfmt, sfmt, sfmt)
    assert len(out) == strip_off, (len(out), strip_off)
    out += body

    with open(path, 'wb') as f:
        f.write(out)
    return len(out)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('output')
    ap.add_argument('--width', type=int, default=1024)
    ap.add_argument('--height', type=int, default=768)
    ap.add_argument('--peak', type=float, default=8.0,
                    help='brightest linear luma, in multiples of SDR white')
    ap.add_argument('--format', choices=['f32', 'u16'], default='f32')
    ap.add_argument('--u16-white', type=float, default=1.0,
                    help='linear value mapped to 65535 when --format u16')
    args = ap.parse_args()

    px = scene(args.width, args.height, args.peak)
    n = write_tiff(args.output, args.width, args.height, px,
                   args.format, args.u16_white)

    above = sum(1 for i in range(0, len(px), 3)
                if 0.2126 * px[i] + 0.7152 * px[i + 1] + 0.0722 * px[i + 2] > 1.0)
    total = args.width * args.height
    print(f'{args.output}: {args.width}x{args.height} {args.format}, {n:,} bytes')
    print(f'  declared peak {args.peak}x SDR white = {math.log2(args.peak):.4f} stops')
    print(f'  {above:,} of {total:,} pixels ({100.0 * above / total:.2f}%) above SDR white')
    if args.format == 'u16':
        print(f'  NOTE: u16 clips above {args.u16_white}x; '
              f'{"lossy for this peak" if args.u16_white < args.peak else "peak fits"}')
    return 0


sys.exit(main())
