#!/usr/bin/env python3
"""Writes a small synthetic PGS .sup (720x576, four cues with known times) to argv[1].

It is the *source* the DVD/DVB subtitle fixtures are encoded from with ffmpeg
(scripts/gen-bitmap-sub-fixtures.sh): ffmpeg's dvdsub and dvbsub encoders take bitmaps only.
No third-party media is involved; the text is rendered with PIL's default font.
"""
import struct, sys
from PIL import Image, ImageDraw, ImageFilter, ImageFont

W, H = 720, 576
CUES = [  # start s, end s, text lines, y
    (0.5, 1.5, ["Hello DVD"], 480),
    (2.0, 3.2, ["Second cue", "line two"], 440),
    (3.5, 4.5, ["Third — éèü"], 60),
    (5.0, 6.0, ["End"], 500),
]
try:
    FONT = ImageFont.load_default(size=36)
except TypeError:
    FONT = ImageFont.load_default()


def seg(kind, pts, body):
    return b"PG" + struct.pack(">IIBH", int(pts * 90000), 0, kind, len(body)) + body


def rle(rows):
    out = bytearray()
    for row in rows:
        x = 0
        while x < len(row):
            c = row[x]
            n = 1
            while x + n < len(row) and row[x + n] == c and n < 16383:
                n += 1
            if c != 0 and n < 3:
                out += bytes([c]) * n
            elif c == 0:
                out += bytes([0, 0x40 | (n >> 8), n & 255]) if n > 63 else bytes([0, n])
            else:
                out += bytes([0, 0xC0 | (n >> 8), n & 255, c]) if n > 63 else bytes([0, 0x80 | n, c])
            x += n
        out += b"\0\0"
    return bytes(out)


def render(lines):
    d = ImageDraw.Draw(Image.new("L", (1, 1)))
    ws = [d.textbbox((0, 0), t, font=FONT)[2] for t in lines]
    w, h = max(ws) + 12, 44 * len(lines) + 8
    fg = Image.new("L", (w, h), 0)
    dr = ImageDraw.Draw(fg)
    for i, t in enumerate(lines):
        dr.text(((w - ws[i]) // 2, 4 + 44 * i), t, fill=255, font=FONT)
    ol = fg.filter(ImageFilter.MaxFilter(5)).filter(ImageFilter.GaussianBlur(0.7))
    px = []
    for y in range(h):
        row = []
        for x in range(w):
            f, o = fg.getpixel((x, y)) // 32, ol.getpixel((x, y)) // 32  # 0..7
            row.append(1 + f if f >= 4 else (9 + o if o else 0)) if False else row.append(
                (1 + f) if f >= 3 else ((9 + o) if o else 0))
        px.append(row)
    return w, h, px


def palette():
    ents = [(0, 16, 128, 128, 0)]
    for i in range(1, 9):   # white ramp, indices 1..8
        ents.append((i, 235, 128, 128, 255 if i > 4 else 128))
    for i in range(9, 17):  # black outline, indices 9..16
        ents.append((i, 16, 128, 128, min(255, 40 * (i - 8))))
    return b"".join(struct.pack("BBBBB", i, y, cr, cb, a) for i, y, cr, cb, a in ents)


out = bytearray()
num = 0
for start, end, lines, y in CUES:
    w, h, px = render(lines)
    x = (W - w) // 2
    data = rle(px)
    pcs = struct.pack(">HHBHBBBB", W, H, 0x10, num, 0x80, 0, 0, 1) + struct.pack(">HBBHH", 0, 0, 0, x, y)
    wds = struct.pack(">BBHHHH", 1, 0, x, y, w, h)
    pds = bytes([0, 0]) + palette()
    ods = struct.pack(">HBB", 0, 0, 0xC0) + (len(data) + 4).to_bytes(3, "big") + struct.pack(">HH", w, h) + data
    for k, b in ((0x16, pcs), (0x17, wds), (0x14, pds), (0x15, ods), (0x80, b"")):
        out += seg(k, start, b)
    num += 1
    pcs = struct.pack(">HHBHBBBB", W, H, 0x10, num, 0x00, 0, 0, 0)
    out += seg(0x16, end, pcs) + seg(0x17, end, struct.pack(">BBHHHH", 1, 0, x, y, w, h)) + seg(0x80, end, b"")
    num += 1
open(sys.argv[1], "wb").write(out)
