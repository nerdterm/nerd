#!/usr/bin/env python3
"""Redraws the two rings of lettering in brand/nerd-logo-{light,dark}.svg as outlines.
Run from the repository root: python3 scripts/logo-rings.py <Archivo-700.ttf> "UPPER RING" "LOWER RING"
then node scripts/brand.mjs for the PNGs.

The font is the static Archivo 700 TTF (OFL, from Google Fonts); it is read, never shipped.
The globe and the NERD wordmark in the SVGs are hand-trimmed and are not touched. Stdlib only."""
import math
import re
import struct
import sys


class Font:
    def __init__(self, path):
        d = self.d = open(path, 'rb').read()
        n = struct.unpack('>H', d[4:6])[0]
        self.t = {d[12 + 16 * i:16 + 16 * i].decode(): struct.unpack('>II', d[20 + 16 * i:28 + 16 * i]) for i in range(n)}
        head = self.t['head'][0]
        self.upm = self.u16(head + 18)
        loc_long = struct.unpack('>h', d[head + 50:head + 52])[0]
        ng = self.u16(self.t['maxp'][0] + 4)
        nh = self.u16(self.t['hhea'][0] + 34)
        hm = self.t['hmtx'][0]
        adv = [self.u16(hm + 4 * i) for i in range(nh)]
        self.adv = adv + [adv[-1]] * (ng - nh)
        lo = self.t['loca'][0]
        self.loca = [struct.unpack('>I', d[lo + 4 * i:lo + 4 * i + 4])[0] if loc_long else self.u16(lo + 2 * i) * 2
                     for i in range(ng + 1)]
        self.cmap = self._cmap()

    def u16(self, o):
        return struct.unpack('>H', self.d[o:o + 2])[0]

    def _cmap(self):
        base = self.t['cmap'][0]
        for i in range(self.u16(base + 2)):
            pid, eid, off = struct.unpack('>HHI', self.d[base + 4 + 8 * i:base + 12 + 8 * i])
            st = base + off
            if pid == 3 and eid == 1 and self.u16(st) == 4:
                seg = self.u16(st + 6) // 2
                ends, starts, deltas, ro = st + 14, st + 16 + seg * 2, st + 16 + seg * 4, st + 16 + seg * 6
                m = {}
                for s in range(seg):
                    e, b = self.u16(ends + 2 * s), self.u16(starts + 2 * s)
                    delta, r = self.u16(deltas + 2 * s), self.u16(ro + 2 * s)
                    for c in range(b, min(e, 0xFFFE) + 1):
                        if r:
                            g = self.u16(ro + 2 * s + r + 2 * (c - b))
                            g = (g + delta) & 0xFFFF if g else 0
                        else:
                            g = (c + delta) & 0xFFFF
                        m[c] = g
                return m
        raise ValueError('no format 4 cmap')

    def contours(self, g):
        """Glyph outline as contours of (x, y, on_curve) in font units."""
        d, o = self.d, self.t['glyf'][0] + self.loca[g]
        if self.loca[g] == self.loca[g + 1]:
            return []
        nc = struct.unpack('>h', d[o:o + 2])[0]
        p = o + 10
        if nc < 0:
            out = []
            while True:
                flags, gi = struct.unpack('>HH', d[p:p + 4]); p += 4
                if flags & 1:
                    dx, dy = struct.unpack('>hh', d[p:p + 4]); p += 4
                else:
                    dx, dy = struct.unpack('>bb', d[p:p + 2]); p += 2
                a, b, c, e = 1, 0, 0, 1
                if flags & 8:
                    a = e = struct.unpack('>h', d[p:p + 2])[0] / 16384; p += 2
                elif flags & 0x40:
                    a, e = (v / 16384 for v in struct.unpack('>hh', d[p:p + 4])); p += 4
                elif flags & 0x80:
                    a, b, c, e = (v / 16384 for v in struct.unpack('>hhhh', d[p:p + 8])); p += 8
                for cont in self.contours(gi):
                    out.append([(a * x + c * y + dx, b * x + e * y + dy, on) for x, y, on in cont])
                if not flags & 0x20:
                    return out
        ends = struct.unpack('>%dH' % nc, d[p:p + 2 * nc]); p += 2 * nc
        p += 2 + self.u16(p)
        npts = ends[-1] + 1
        flags = []
        while len(flags) < npts:
            f = d[p]; p += 1
            flags.append(f)
            if f & 8:
                flags += [f] * d[p]; p += 1
        coords = []
        for short, same in ((2, 16), (4, 32)):
            v, vals = 0, []
            for f in flags[:npts]:
                if f & short:
                    delta = d[p]; p += 1
                    v += delta if f & same else -delta
                elif not f & same:
                    v += struct.unpack('>h', d[p:p + 2])[0]; p += 2
                vals.append(v)
            coords.append(vals)
        pts = [(coords[0][i], coords[1][i], bool(flags[i] & 1)) for i in range(npts)]
        out, s = [], 0
        for e in ends:
            out.append(pts[s:e + 1]); s = e + 1
        return out

    def glyph(self, ch):
        # A missing character would otherwise be drawn as the font's empty box.
        g = self.cmap.get(ord(ch), 0)
        if not g:
            sys.exit(f'the font has no glyph for {ch!r}')
        return g

    def cap(self):
        g = self.glyph('H')
        return struct.unpack('>h', self.d[self.t['glyf'][0] + self.loca[g] + 8:][:2])[0]


def path(conts, tf):
    """TrueType quadratic contours to SVG path data, points mapped through tf."""
    f = lambda p: '%.1f %.1f' % tf(p[0], p[1])
    out = []
    for c in conts:
        if not c:
            continue
        pts = c[:]
        if not pts[0][2]:
            last = pts[-1]
            start = last if last[2] else ((last[0] + pts[0][0]) / 2, (last[1] + pts[0][1]) / 2, True)
            if last[2]:
                pts = pts[:-1]
            pts = [start] + pts
        s = 'M' + f(pts[0])
        i, n = 1, len(pts)
        while i <= n:
            p = pts[i % n] if i < n else pts[0]
            if p[2]:
                s += 'L' + f(p); i += 1
            else:
                q = pts[(i + 1) % n] if i + 1 < n else pts[0]
                end = q if q[2] else ((p[0] + q[0]) / 2, (p[1] + q[1]) / 2, True)
                s += 'Q' + f(p) + ' ' + f(end)
                i += 2 if q[2] else 1
                if not q[2] and i > n:
                    break
        out.append(s + 'Z')
    return ''.join(out)


def arc_len(rx, ry, deg):
    n, T = 400, math.radians(deg)
    return 2 * sum(math.hypot(rx * math.cos((i + .5) * T / n), ry * math.sin((i + .5) * T / n)) * T / n for i in range(n))


def edge_angle(rx, ry, edge, cap, top):
    x = lambda t: rx * math.sin(t) + (cap * ry * math.sin(t) / math.hypot(ry * math.sin(t), rx * math.cos(t)) if top else 0)
    lo, hi = 0, math.pi / 2
    for _ in range(50):
        m = (lo + hi) / 2
        lo, hi = (m, hi) if x(m) < edge else (lo, m)
    return math.degrees(lo)


def arc_text(font, text, cx, cy, rx, ry, gap, size, top, edge):
    """Text on the rim: top caps grow outward, bottom caps inward; both ends reach x = ±edge."""
    s = size / font.upm
    cap = font.cap() * s
    off = gap + (0 if top else cap)
    rx, ry = rx + off, ry + off
    total = arc_len(rx, ry, edge_angle(rx, ry, edge, cap, top))
    # Sample the half ellipse from the left end, through the pole, to the right end.
    n = 6000
    pts = [(cx + rx * math.cos(math.pi - math.pi * i / n), cy + (-1 if top else 1) * ry * math.sin(math.pi * i / n)) for i in range(n + 1)]
    cum = [0.0]
    for a, b in zip(pts, pts[1:]):
        cum.append(cum[-1] + math.dist(a, b))

    def at(L):
        lo, hi = 0, n
        while hi - lo > 1:
            m = (lo + hi) // 2
            lo, hi = (m, hi) if cum[m] <= L else (lo, m)
        t = (L - cum[lo]) / (cum[hi] - cum[lo])
        (x0, y0), (x1, y1) = pts[lo], pts[hi]
        return x0 + (x1 - x0) * t, y0 + (y1 - y0) * t, math.atan2(y1 - y0, x1 - x0)

    gs = [font.glyph(c) for c in text]
    advs = [font.adv[g] * s for g in gs]
    # The letters are spread to fill the arc, so a ring far from 26-36 characters reads cramped or loose.
    extra = (total - sum(advs)) / (len(gs) - 1)
    pos, d = cum[-1] / 2 - total / 2, ''
    for g, a in zip(gs, advs):
        px, py, th = at(pos + a / 2)
        c, sn = math.cos(th), math.sin(th)
        def tf(u, v, a=a, px=px, py=py, c=c, sn=sn):
            lx, ly = u * s - a / 2, -v * s
            return px + lx * c - ly * sn, py + lx * sn + ly * c
        d += path(font.contours(g), tf)
        pos += a + extra
    return d


if len(sys.argv) != 4:
    sys.exit(__doc__)
font, upper, lower = Font(sys.argv[1]), sys.argv[2], sys.argv[3]
# The globe of the lockup: centre, radii, gap to the rim, letter size, and the x both rings end at.
G = dict(cx=500, cy=230, rx=440, ry=150, gap=22, size=30, edge=380)
rings = arc_text(font, upper, top=True, **G) + arc_text(font, lower, top=False, **G)
for theme in ('light', 'dark'):
    name = f'brand/nerd-logo-{theme}.svg'
    svg = open(name).read()
    # The lettering is the last path in the file; the wordmark and its mask come before it.
    last = re.findall(r'<path d="[^"]+"[^>]*/>', svg)[-1]
    open(name, 'w').write(svg.replace(last, re.sub(r'd="[^"]+"', f'd="{rings}"', last, count=1)))
    print(name)
