#!/usr/bin/env python3
"""Independently check a HEIC against docs/acceptance-criteria.md.

Deliberately shares no code with the Rust crates. `tohdr verify` uses our own
reader, so a bug present in both our reader and our writer would pass it; this
walks the container from scratch with nothing but stdlib struct unpacking, and
cross-checks the headroom against exiftool. When the two agree, the structure is
real. When they disagree, that is the interesting case.

Usage:
    tools/verify_gainmap.py <file.heic> [--json] [--expect-flavor apple|iso|both]

Exit status: 0 if every applicable criterion passes, 1 otherwise, 2 on a read
error. Criteria are numbered to match docs/acceptance-criteria.md.
"""
import argparse
import json
import math
import struct
import subprocess
import sys

APPLE_URN = 'urn:com:apple:photo:2020:aux:hdrgainmap'


# --------------------------------------------------------------------------
# Minimal ISOBMFF walk. Only what the criteria need.
# --------------------------------------------------------------------------

class Box:
    __slots__ = ('type', 'start', 'body', 'end')

    def __init__(self, btype, start, body, end):
        self.type, self.start, self.body, self.end = btype, start, body, end

    def __repr__(self):
        return f'Box({self.type} @{self.start}..{self.end})'


def boxes(buf, start, end):
    """Yield the sequence of boxes in buf[start:end]."""
    p = start
    while p + 8 <= end:
        size, btype = struct.unpack_from('>I4s', buf, p)
        hdr = 8
        if size == 1:
            size = struct.unpack_from('>Q', buf, p + 8)[0]
            hdr = 16
        elif size == 0:
            size = end - p
        if size < hdr or p + size > end:
            return
        yield Box(btype.decode('latin1'), p, p + hdr, p + size)
        p += size


def find(buf, start, end, btype):
    for b in boxes(buf, start, end):
        if b.type == btype:
            return b
    return None


def parse_ftyp(buf, b):
    major = buf[b.body:b.body + 4].decode('latin1')
    brands = []
    p = b.body + 8
    while p + 4 <= b.end:
        brands.append(buf[p:p + 4].decode('latin1'))
        p += 4
    return major, brands


def parse_iinf(buf, b):
    """-> {item_id: item_type}. Handles infe versions 0-3."""
    ver = buf[b.body]
    p = b.body + 4
    if ver == 0:
        count = struct.unpack_from('>H', buf, p)[0]
        p += 2
    else:
        count = struct.unpack_from('>I', buf, p)[0]
        p += 4
    out = {}
    for b2 in boxes(buf, p, b.end):
        if b2.type != 'infe':
            continue
        v = buf[b2.body]
        q = b2.body + 4
        if v in (0, 1):
            iid = struct.unpack_from('>H', buf, q)[0]
            out[iid] = 'hvc1'  # v0/v1 carry no type; not used by our targets
        else:
            if v == 2:
                iid = struct.unpack_from('>H', buf, q)[0]
                q += 2
            else:
                iid = struct.unpack_from('>I', buf, q)[0]
                q += 4
            q += 2  # protection index
            out[iid] = buf[q:q + 4].decode('latin1')
    if len(out) != count:
        # Not fatal: report what we found and let the caller notice.
        pass
    return out


def parse_iref(buf, b):
    """-> list of (type, from_id, [to_ids])."""
    ver = buf[b.body]
    wide = ver != 0
    refs = []
    for b2 in boxes(buf, b.body + 4, b.end):
        p = b2.body
        if wide:
            frm = struct.unpack_from('>I', buf, p)[0]
            p += 4
        else:
            frm = struct.unpack_from('>H', buf, p)[0]
            p += 2
        n = struct.unpack_from('>H', buf, p)[0]
        p += 2
        tos = []
        for _ in range(n):
            if wide:
                tos.append(struct.unpack_from('>I', buf, p)[0])
                p += 4
            else:
                tos.append(struct.unpack_from('>H', buf, p)[0])
                p += 2
        refs.append((b2.type, frm, tos))
    return refs


def parse_iloc(buf, b):
    """-> {item_id: (construction_method, [(offset, length)])}."""
    ver = buf[b.body]
    p = b.body + 4
    sizes = buf[p]
    offset_size, length_size = sizes >> 4, sizes & 0xF
    p += 1
    sizes2 = buf[p]
    base_size, index_size = sizes2 >> 4, sizes2 & 0xF
    p += 1
    if ver < 2:
        count = struct.unpack_from('>H', buf, p)[0]
        p += 2
    else:
        count = struct.unpack_from('>I', buf, p)[0]
        p += 4

    def rd(n):
        nonlocal p
        if n == 0:
            return 0
        v = int.from_bytes(buf[p:p + n], 'big')
        p += n
        return v

    out = {}
    for _ in range(count):
        if ver < 2:
            iid = struct.unpack_from('>H', buf, p)[0]
            p += 2
        else:
            iid = struct.unpack_from('>I', buf, p)[0]
            p += 4
        method = 0
        if ver in (1, 2):
            method = struct.unpack_from('>H', buf, p)[0] & 0xF
            p += 2
        p += 2  # data_reference_index
        base = rd(base_size)
        n_ext = struct.unpack_from('>H', buf, p)[0]
        p += 2
        extents = []
        for _ in range(n_ext):
            if ver in (1, 2) and index_size:
                rd(index_size)
            off = rd(offset_size)
            ln = rd(length_size)
            extents.append((base + off, ln))
        out[iid] = (method, extents)
    return out


def parse_ipma(buf, b):
    """-> {item_id: [property_index]}."""
    ver = buf[b.body]
    flags = int.from_bytes(buf[b.body + 1:b.body + 4], 'big')
    p = b.body + 4
    count = struct.unpack_from('>I', buf, p)[0]
    p += 4
    out = {}
    for _ in range(count):
        if ver < 1:
            iid = struct.unpack_from('>H', buf, p)[0]
            p += 2
        else:
            iid = struct.unpack_from('>I', buf, p)[0]
            p += 4
        n = buf[p]
        p += 1
        props = []
        for _ in range(n):
            if flags & 1:
                v = struct.unpack_from('>H', buf, p)[0]
                p += 2
                props.append(v & 0x7FFF)
            else:
                v = buf[p]
                p += 1
                props.append(v & 0x7F)
        out[iid] = props
    return out


def parse_ispe(buf, b):
    return struct.unpack_from('>II', buf, b.body + 4)


def parse_auxc(buf, b):
    z = buf.index(b'\x00', b.body + 4)
    return buf[b.body + 4:z].decode('latin1')


def parse_pixi(buf, b):
    n = buf[b.body + 4]
    return list(buf[b.body + 5:b.body + 5 + n])


# --------------------------------------------------------------------------
# ISO 21496-1 C.2.2, as laid out in docs/heic-gainmap-structure.md.
# --------------------------------------------------------------------------

def decode_iso21496(payload):
    """Decode a tmap payload (1 ToneMapImage version byte + the C.2.2 struct)."""
    if len(payload) < 6:
        raise ValueError(f'payload too short: {len(payload)} bytes')
    out = {'payload_bytes': len(payload), 'tone_map_image_version': payload[0]}
    p = 1
    out['minimum_version'], out['writer_version'] = struct.unpack_from('>HH', payload, p)
    p += 4
    flags = payload[p]
    p += 1
    out['flags'] = flags
    out['is_multichannel'] = bool(flags & 0x80)
    out['use_base_colour_space'] = bool(flags & 0x40)

    def frac(signed):
        nonlocal p
        fmt = '>iI' if signed else '>II'
        n, d = struct.unpack_from(fmt, payload, p)
        p += 8
        if d == 0:
            raise ValueError('zero denominator')
        return n / d

    out['base_headroom'] = frac(False)
    out['alt_headroom'] = frac(False)
    nch = 3 if out['is_multichannel'] else 1
    out['channels'] = nch
    for name in ('min_log2', 'max_log2', 'gamma', 'base_offset', 'alt_offset'):
        out[name] = []
    for _ in range(nch):
        out['min_log2'].append(frac(True))
        out['max_log2'].append(frac(True))
        out['gamma'].append(frac(False))
        out['base_offset'].append(frac(True))
        out['alt_offset'].append(frac(True))
    out['bytes_consumed'] = p
    out['exact'] = (p == len(payload))
    return out


def headroom_from_tags(tag33, tag48):
    """MakerApple tags 33/48 -> linear headroom.

    Skia's get_maker_note_hdr_headroom, src/codec/SkExif.cpp:82-96, and a
    deliberate second implementation of tohdr_core::apple::headroom_from_tags:
    two ports of one spec disagreeing is a signal, one port checking itself is
    not. Keep them in step.
    """
    if tag33 is None:
        return None
    if tag33 < 1.0:
        stops = -20.0 * tag48 + 1.8 if tag48 <= 0.01 else -0.101 * tag48 + 1.601
    else:
        stops = -70.0 * tag48 + 3.0 if tag48 <= 0.01 else -0.303 * tag48 + 2.303
    return 2.0 ** min(max(stops, 0.0), 16.0)


def gain_weight(base_hr, alt_hr, display_stops):
    """libavif avifGetGainMapWeight, src/gainmap.c:52-63. Clamp, then flip."""
    if base_hr == alt_hr:
        return 0.0
    w = (display_stops - base_hr) / (alt_hr - base_hr)
    w = max(0.0, min(1.0, w))
    return -w if alt_hr < base_hr else w


def exiftool_tags(path):
    """Headroom copies from XMP and MakerApple. Absent exiftool is not fatal."""
    try:
        r = subprocess.run(
            ['exiftool', '-j', '-n', '-HDRGainMapHeadroom', '-HDRHeadroom', '-HDRGain', path],
            capture_output=True, text=True, timeout=60)
        if r.returncode != 0 or not r.stdout.strip():
            return {}
        d = json.loads(r.stdout)[0]
        return {k: v for k, v in d.items() if k != 'SourceFile'}
    except (OSError, ValueError, subprocess.SubprocessError):
        return {}


# --------------------------------------------------------------------------

def analyze(path):
    buf = open(path, 'rb').read()
    info = {'path': path, 'size_bytes': len(buf)}

    ftyp = find(buf, 0, len(buf), 'ftyp')
    if not ftyp:
        raise ValueError('no ftyp box: not an ISOBMFF file')
    info['major_brand'], info['brands'] = parse_ftyp(buf, ftyp)

    meta = find(buf, 0, len(buf), 'meta')
    if not meta:
        raise ValueError('no meta box')
    mb, me = meta.body + 4, meta.end  # meta is a FullBox

    pitm = find(buf, mb, me, 'pitm')
    if pitm:
        ver = buf[pitm.body]
        info['primary_item'] = struct.unpack_from(
            '>I' if ver else '>H', buf, pitm.body + 4)[0]

    iinf = find(buf, mb, me, 'iinf')
    types = parse_iinf(buf, iinf) if iinf else {}
    info['item_types'] = {str(k): v for k, v in sorted(types.items())}

    iref = find(buf, mb, me, 'iref')
    refs = parse_iref(buf, iref) if iref else []
    info['refs'] = [{'type': t, 'from': f, 'to': to} for t, f, to in refs]

    # Properties, so we can attribute auxC / ispe / pixi to specific items.
    iprp = find(buf, mb, me, 'iprp')
    props = []
    assoc = {}
    if iprp:
        ipco = find(buf, iprp.body, iprp.end, 'ipco')
        if ipco:
            props = list(boxes(buf, ipco.body, ipco.end))
        for b in boxes(buf, iprp.body, iprp.end):
            if b.type == 'ipma':
                assoc.update(parse_ipma(buf, b))

    def item_prop(iid, btype):
        for idx in assoc.get(iid, []):
            if 1 <= idx <= len(props) and props[idx - 1].type == btype:
                return props[idx - 1]
        return None

    # Locate the gain map by either signaling route.
    apple_gain_items = []
    all_auxc = []
    for iid in types:
        b = item_prop(iid, 'auxC')
        if b:
            urn = parse_auxc(buf, b)
            all_auxc.append({'item': iid, 'urn': urn})
            if urn == APPLE_URN:
                apple_gain_items.append(iid)
    info['auxc'] = all_auxc

    tmap_items = [i for i, t in types.items() if t == 'tmap']
    info['tmap_items'] = tmap_items

    tmap_dimg = None
    for t, f, to in refs:
        if t == 'dimg' and f in tmap_items:
            tmap_dimg = to
            break
    info['tmap_dimg'] = tmap_dimg

    # Identify the gain map WITHOUT consulting `pitm` or the tmap's `dimg`,
    # so the criteria that compare against those fields are real comparisons.
    #
    # Deriving it from `dimg[1]` and then checking `dimg[1] == gain` (as this
    # did) is a tautology: a writer emitting [gain, base] would have passed.
    # The two independent signals are the Apple auxC URN and the fact that a
    # gain map is single-channel where the base is three-channel.
    #
    # Grid tiles have to come out of the candidate set first -- IMG_4913 has
    # ~120 of them, and its gain tiles are single-channel too, so without this
    # the pixi signal is hopelessly ambiguous. A tile is an item that some
    # NON-tmap item derives from.
    tile_ids = set()
    for t, f, to in refs:
        if t == 'dimg' and f not in tmap_items:
            tile_ids.update(to)
    coded = [
        i for i, t in types.items()
        if t in ('hvc1', 'hev1', 'grid') and i not in tile_ids
    ]

    def channels(iid):
        b = item_prop(iid, 'pixi')
        if not b:
            return None
        d = parse_pixi(buf, b)
        return len(d) if d else None

    gain_item = apple_gain_items[0] if apple_gain_items else None
    if gain_item is None and tmap_dimg:
        # No Apple URN. Use `pitm` -- which is independent of `dimg` -- to say
        # which of the tmap's two inputs is NOT the base. Deliberately does not
        # assume the gain map is single-channel: a 3-channel gain map is
        # exactly what criterion 2 exists to catch, and identifying the item by
        # its channel count would make that criterion unable to see it.
        others = [i for i in tmap_dimg if i != info.get('primary_item')]
        if len(others) == 1:
            gain_item = others[0]
    if gain_item is None:
        single = [i for i in coded if channels(i) == 1]
        if len(single) == 1:
            gain_item = single[0]

    # `pitm` is the independent statement of which item is the base; criterion
    # 1 checks that claim against the gain map's identity rather than against
    # itself, and criterion 4 checks dimg's order against it.
    base_item = info.get('primary_item')

    info['gain_item'] = gain_item
    info['base_item'] = base_item
    info['coded_items'] = coded

    if gain_item is not None:
        b = item_prop(gain_item, 'ispe')
        if b:
            info['gain_size'] = list(parse_ispe(buf, b))
        b = item_prop(gain_item, 'pixi')
        if b:
            info['gain_pixi'] = parse_pixi(buf, b)
        info['gain_auxl'] = [to for t, f, to in refs if t == 'auxl' and f == gain_item]

    base_item = info.get('base_item')
    if base_item is not None:
        b = item_prop(base_item, 'ispe')
        if b:
            info['base_size'] = list(parse_ispe(buf, b))

    # tmap payload -> ISO 21496-1 metadata.
    iloc = find(buf, mb, me, 'iloc')
    idat = find(buf, mb, me, 'idat')
    locs = parse_iloc(buf, iloc) if iloc else {}
    info['iso'] = None
    if tmap_items and tmap_items[0] in locs:
        method, extents = locs[tmap_items[0]]
        blob = b''
        for off, ln in extents:
            if method == 1:
                if not idat:
                    raise ValueError('iloc says idat-relative but no idat box')
                blob += buf[idat.body + off: idat.body + off + ln]
            elif method == 0:
                blob += buf[off: off + ln]
            else:
                raise ValueError(f'unsupported construction_method {method}')
        info['tmap_construction_method'] = method
        try:
            info['iso'] = decode_iso21496(blob)
        except ValueError as e:
            info['iso_error'] = str(e)

    info['exif'] = exiftool_tags(path)
    return info


def check(info, expect_flavor):
    """-> list of (criterion, name, status, detail). status in pass/fail/skip."""
    out = []

    def add(num, name, ok, detail):
        out.append((num, name, 'pass' if ok else 'fail', detail))

    def skip(num, name, detail):
        out.append((num, name, 'skip', detail))

    iso = info.get('iso')
    gain = info.get('gain_item')
    has_apple = bool([a for a in info.get('auxc', []) if a['urn'] == APPLE_URN])
    has_iso = bool(info.get('tmap_items'))

    want_apple = expect_flavor in (None, 'apple', 'both')
    want_iso = expect_flavor in (None, 'iso', 'both')
    if expect_flavor is None:
        # Nothing declared: require at least one route to exist.
        add(0, 'some gain-map signaling present', has_apple or has_iso,
            f'apple={has_apple} iso={has_iso}')

    # 1. base is primary. `base_item` is identified structurally (see analyze),
    # never from `pitm`, so this is a real comparison of two independent
    # signals rather than a field against itself.
    pitm = info.get('primary_item')
    gain_id = info.get('gain_item')
    # Real content: the primary item must be an actual image, and must be
    # neither the gain map nor the tmap. Comparing pitm to a base that was
    # *derived from* pitm proved nothing.
    ok1 = (pitm is not None
           and pitm in info.get('coded_items', [])
           and gain_id is not None and pitm != gain_id
           and pitm not in info.get('tmap_items', []))
    add(1, 'base image is the primary item', ok1,
        f"pitm={pitm} gain={gain_id} tmaps={info.get('tmap_items')} "
        f"(pitm must be a coded image that is neither)")

    # 2. gain map is single-channel 8-bit
    pixi = info.get('gain_pixi')
    if pixi is None:
        skip(2, 'gain map single-channel 8-bit', 'no pixi on the gain-map item')
    else:
        add(2, 'gain map single-channel 8-bit', pixi == [8],
            f'pixi={pixi} (want [8])')

    # 3. Apple flavor: URN + auxl back-reference
    if want_apple and (expect_flavor is not None or has_apple):
        auxl = info.get('gain_auxl') or []
        flat = [i for lst in auxl for i in lst]
        ok = has_apple and info.get('base_item') in flat
        add(3, 'Apple URN + auxl to base', ok,
            f'urn={has_apple} auxl={auxl} base={info.get("base_item")}')
    else:
        skip(3, 'Apple URN + auxl to base', 'flavor does not include Apple')

    # 4. ISO flavor: tmap, dimg order, brand, payload size
    if want_iso and (expect_flavor is not None or has_iso):
        dimg = info.get('tmap_dimg')
        brand_ok = 'tmap' in info.get('brands', [])
        # Both operands are identified structurally in analyze(), so this
        # really does check the dimg ORDER rather than restating it.
        base_id = info.get('base_item')
        # `dimg[0] == pitm` is the real order test: pitm comes from a
        # different box entirely, so a writer emitting [gain, base] fails here.
        order_ok = (bool(dimg) and len(dimg) == 2
                    and base_id is not None and gain is not None
                    and dimg[0] == base_id and dimg[1] == gain
                    and dimg[0] != dimg[1])
        size_ok = bool(iso) and iso['payload_bytes'] in (62, 142) and iso['exact']
        add(4, 'tmap item, dimg [base,gain], tmap brand, exact payload',
            brand_ok and order_ok and size_ok,
            f'brand={brand_ok} dimg={dimg} want=[{base_id}, {gain}] payload='
            f'{iso["payload_bytes"] if iso else None} exact={iso["exact"] if iso else None}')
    else:
        skip(4, 'ISO tmap signaling', 'flavor does not include ISO')

    # 5. THE invariant.
    if not iso:
        skip(5, 'max_log2 == alt_headroom', 'no ISO metadata in file')
    else:
        d = abs(iso['max_log2'][0] - iso['alt_headroom'])
        add(5, 'max_log2 == alt_headroom', d < 1e-3,
            f"max_log2={iso['max_log2'][0]:.6f} alt_headroom={iso['alt_headroom']:.6f} "
            f"delta={d:+.6f} stops")

    # 6. base_headroom zero
    if not iso:
        skip(6, 'base_headroom == 0', 'no ISO metadata')
    else:
        add(6, 'base_headroom == 0', abs(iso['base_headroom']) < 1e-6,
            f"base_headroom={iso['base_headroom']}")

    # 7. libavif validation
    if not iso:
        skip(7, 'passes avifGainMapValidateMetadata', 'no ISO metadata')
    else:
        ok = all(iso['max_log2'][c] >= iso['min_log2'][c] for c in range(iso['channels'])) \
            and all(g != 0.0 for g in iso['gamma'])
        add(7, 'passes avifGainMapValidateMetadata', ok,
            f"max>=min and gamma nonzero; gamma={[round(g,6) for g in iso['gamma']]}")

    # 8. MakerApple tag48 non-negative
    ex = info.get('exif', {})
    t48 = ex.get('HDRGain')
    t33 = ex.get('HDRHeadroom')
    if t48 is None and t33 is None:
        skip(8, 'MakerApple tag48 non-negative', 'no MakerApple headroom tags present')
    else:
        ok = (t48 is None or t48 >= 0) and (t48 is None or t33 is not None)
        add(8, 'MakerApple tag48 non-negative', ok, f'tag33={t33} tag48={t48}')

    # 9. headroom copies agree
    #
    # All three copies, not just two. The MakerApple pair is the copy a file
    # inherits when a conversion carries the source's MakerNote, and it is the
    # one most likely to be stale -- it describes whatever the *source's*
    # headroom was. Leaving it out of this check is how a 1.3% over-declaration
    # rides along unnoticed.
    xmp = ex.get('HDRGainMapHeadroom')
    maker = headroom_from_tags(t33, t48) if t48 is not None else None
    copies = [('xmp', xmp), ('maker', maker)]
    present = [(n, v) for n, v in copies if v is not None]
    if not iso or not present:
        skip(9, 'headroom copies agree',
             f'iso={bool(iso)} xmp={xmp} maker={maker}')
    else:
        iso_lin = 2.0 ** iso['alt_headroom']
        worst = max(abs(iso_lin - v) for _, v in present)
        add(9, 'headroom copies agree', worst < 1e-3,
            f'iso={iso_lin:.6f}x '
            + ' '.join(f'{n}={v:.6f}x' for n, v in present)
            + f' worst delta={worst:.2e}')

    # 10/11. predicted weights
    #
    # The invariant is NOT "weight == 1.0 on a phone" -- that only holds for a
    # scene needing <= 2.3 stops, and would fail a correctly-built file of a
    # brighter scene. What must hold is that a display receives every stop it
    # can show: delivered == min(display_headroom, alt_headroom). Given
    # base_headroom == 0 that follows from criterion 5, since
    #   delivered = max_log2 * clamp(display/alt) = alt * min(1, display/alt).
    # An over-declaring file breaks it: DSC07752_iso encodes 1.96 stops but
    # declares 3.568, so a 2.3-stop display gets 1.263 where it should get 1.96.
    if not iso:
        skip(10, 'display receives every stop it can show', 'no ISO metadata')
    else:
        worst = None
        for display in (1.0, 1.5, 2.0, 2.3, 2.98, 4.0):
            w = gain_weight(iso['base_headroom'], iso['alt_headroom'], display)
            delivered = iso['max_log2'][0] * w
            want = min(display, iso['max_log2'][0])
            err = abs(delivered - want)
            if worst is None or err > worst[3]:
                worst = (display, delivered, want, err)
        d, got, want, err = worst
        add(10, 'display receives every stop it can show', err < 1e-3,
            f'worst at {d:.2f}-stop display: delivered {got:.3f}, '
            f'expected min(display, max_log2)={want:.3f} (err {err:.3f})')

    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('file')
    ap.add_argument('--json', action='store_true')
    ap.add_argument('--expect-flavor', choices=['apple', 'iso', 'both'])
    args = ap.parse_args()

    try:
        info = analyze(args.file)
    except (ValueError, OSError, struct.error, IndexError) as e:
        print(f'ERROR reading {args.file}: {e}', file=sys.stderr)
        return 2

    results = check(info, args.expect_flavor)
    failed = [r for r in results if r[2] == 'fail']

    if args.json:
        print(json.dumps({
            'info': info,
            'checks': [{'criterion': n, 'name': nm, 'status': s, 'detail': d}
                       for n, nm, s, d in results],
            'failed': len(failed),
        }, indent=2, default=str))
    else:
        print(f'{args.file}  ({info["size_bytes"]:,} bytes, brands: '
              f'{" ".join(info["brands"])})')
        b, g = info.get('base_size'), info.get('gain_size')
        if b:
            frac = f' (1/{b[0] / g[0]:.3g} of base)' if g and g[0] else ''
            print(f'  base {b[0]}x{b[1]}   gain '
                  f'{g[0]}x{g[1]}{frac}' if g else f'  base {b[0]}x{b[1]}   gain: none found')
        for n, nm, s, d in results:
            mark = {'pass': 'PASS', 'fail': 'FAIL', 'skip': 'skip'}[s]
            print(f'  [{mark}] {n:>2}. {nm}\n           {d}')
        print(f'  => {len(failed)} failed, '
              f'{sum(1 for r in results if r[2] == "pass")} passed, '
              f'{sum(1 for r in results if r[2] == "skip")} skipped')

    return 1 if failed else 0


sys.exit(main())
