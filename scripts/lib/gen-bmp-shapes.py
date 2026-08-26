"""Write the BMP shapes ImageMagick will not emit.

ImageMagick covers the ordinary files (1/4/8-bit palettes, RLE8, 24-bit), but
it has no switch for a 16-bit bitfield file, a top-down row order, a 32-bit
file with an alpha channel, or the 12-byte OS/2 header -- all of which are
shapes a decoder meets in the wild.  Each is written here from a raw RGBA dump
so the pixels are the same picture as the rest of the corpus.
"""

import struct
import sys

out, raw, width, height = sys.argv[1], sys.argv[2], int(sys.argv[3]), int(sys.argv[4])
rgba = open(raw, "rb").read()
rgba_alpha = open(sys.argv[5], "rb").read()


def pixel(source, x, y):
    at = (y * width + x) * 4
    return source[at], source[at + 1], source[at + 2], source[at + 3]


def rows(top_down, source=None):
    source = rgba if source is None else source
    order = range(height) if top_down else range(height - 1, -1, -1)
    return [[pixel(source, x, y) for x in range(width)] for y in order]


def file_header(offset, size):
    return b"BM" + struct.pack("<IHHI", size, 0, 0, offset)


def info_v3(bits, compression, masks=()):
    header = struct.pack(
        "<IiiHHIIiiII", 40, width, -height if masks == "topdown" else height,
        1, bits, compression, 0, 2835, 2835, 0, 0,
    )
    return header


def write(name, info, body, extra=b""):
    offset = 14 + len(info) + len(extra)
    blob = file_header(offset, offset + len(body)) + info + extra + body
    open(f"{out}/{name}", "wb").write(blob)


def packed(bits_per_pixel, encode, top_down=False, source=None):
    """Pack rows to the format's four-byte row alignment."""
    stride = ((width * bits_per_pixel + 31) // 32) * 4
    body = bytearray()
    for row in rows(top_down, source):
        line = bytearray()
        for pix in row:
            line += encode(pix)
        line += b"\0" * (stride - len(line))
        body += line
    return bytes(body)


# 16-bit, 5-6-5, declared through BI_BITFIELDS masks.
masks565 = struct.pack("<III", 0xF800, 0x07E0, 0x001F)
body = packed(16, lambda p: struct.pack(
    "<H", ((p[0] >> 3) << 11) | ((p[1] >> 2) << 5) | (p[2] >> 3)))
write("bf565.bmp", info_v3(16, 3), body, masks565)

# 16-bit, 5-5-5, the layout BI_RGB implies at this depth (no mask block).
body = packed(16, lambda p: struct.pack(
    "<H", ((p[0] >> 3) << 10) | ((p[1] >> 3) << 5) | (p[2] >> 3)))
write("rgb555.bmp", info_v3(16, 0), body)

# 32-bit with an alpha channel: the fourth mask only has a home in the
# BITMAPV4HEADER, whose 108 bytes carry all four masks plus a colour space.
body = packed(32, lambda p: bytes([p[2], p[1], p[0], p[3]]), source=rgba_alpha)
v4 = struct.pack(
    "<IiiHHIIiiII", 108, width, height, 1, 32, 3, len(body), 2835, 2835, 0, 0,
) + struct.pack(
    "<IIII", 0x00FF0000, 0x0000FF00, 0x000000FF, 0xFF000000,
) + b"BGRs" + b"\0" * 48
write("alpha32.bmp", v4, body)

# Top-down rows: a negative height, first row stored first.
body = packed(24, lambda p: bytes([p[2], p[1], p[0]]), top_down=True)
info = struct.pack("<IiiHHIIiiII", 40, width, -height, 1, 24, 0, 0, 2835, 2835, 0, 0)
write("topdown24.bmp", info, body)

# RLE4, from the 4-bit palette file ImageMagick wrote: same palette, same
# indices, only the run encoding differs -- ImageMagick's RLE switch produces
# an 8-bit file whatever the colour count, so the 4-bit run path has no other
# source.
def rle4(source_name, name):
    src = open(f"{out}/{source_name}", "rb").read()
    offset, = struct.unpack_from("<I", src, 10)
    w, h = struct.unpack_from("<ii", src, 18)
    colours, = struct.unpack_from("<I", src, 46)
    colours = colours or 16
    palette = src[14 + 40:14 + 40 + colours * 4]
    stride = ((w * 4 + 31) // 32) * 4
    body = bytearray()
    for y in range(h):
        line = src[offset + y * stride:offset + (y + 1) * stride]
        indices = []
        for x in range(w):
            byte = line[x // 2]
            indices.append(byte >> 4 if x % 2 == 0 else byte & 0x0F)
        x = 0
        while x < w:
            run = 1
            while x + run < w and run < 254 and indices[x + run] == indices[x]:
                run += 1
            if run >= 2:
                body += bytes([run, indices[x] << 4 | indices[x]])
                x += run
                continue
            # A literal run needs at least three pixels to be legal; below that
            # the encoded form is a run of one.
            end = x
            while end < w and end - x < 254:
                same = 1
                while end + same < w and indices[end + same] == indices[end]:
                    same += 1
                if same >= 3:
                    break
                end += same
            count = end - x
            if count < 3:
                body += bytes([1, indices[x] << 4])
                x += 1
                continue
            nibbles = bytearray()
            for i in range(0, count, 2):
                high = indices[x + i] << 4
                low = indices[x + i + 1] if i + 1 < count else 0
                nibbles.append(high | low)
            while len(nibbles) % 2:
                nibbles.append(0)
            body += bytes([0, count]) + nibbles
            x = end
        body += b"\x00\x00"
    body = body[:-2] + b"\x00\x01"
    info = struct.pack("<IiiHHIIiiII", 40, w, h, 1, 4, 2, len(body), 2835, 2835,
                       colours, 0)
    write(name, info, bytes(body), palette)


rle4("pal4.bmp", "rle4.bmp")

# BI_PNG and BI_JPEG: a BMP file header and info header wrapped around a whole
# file of another format, which is what a printer driver or a clipboard writes.
def wrap(name, payload_path, compression):
    payload = open(payload_path, "rb").read()
    info = struct.pack(
        "<IiiHHIIiiII", 40, width, height, 1, 0, compression, len(payload),
        2835, 2835, 0, 0,
    )
    write(name, info, payload)


wrap("embedded-png.bmp", sys.argv[6], 5)
wrap("embedded-jpeg.bmp", sys.argv[7], 4)

# The OS/2 v2 header, in the 16-byte form writers actually emit: 32-bit
# dimensions like a Windows header, and nothing after them.
body = packed(24, lambda p: bytes([p[2], p[1], p[0]]))
write("os2v2.bmp", struct.pack("<IiiHH", 16, width, height, 1, 24), body)

# The 12-byte OS/2 v1 header, whose dimensions are 16-bit and unsigned.
body = packed(24, lambda p: bytes([p[2], p[1], p[0]]))
write("os2v1.bmp", struct.pack("<IhhHH", 12, width, height, 1, 24), body)
