"""Write the TIFF alpha shapes ImageMagick will not emit.

ImageMagick always tags a TIFF's alpha channel as associated (ExtraSamples 2,
premultiplied) and ignores ``-define tiff:alpha=unassociated``, so the ordinary
straight-alpha file -- the common one -- has to be written here.  Both files
carry the same picture with the alpha flattened to 0 or 255, which makes the
premultiplication lossless and so lets the two decode to identical pixels.
"""

import struct
import sys

out, raw, width, height = sys.argv[1], sys.argv[2], int(sys.argv[3]), int(sys.argv[4])
rgba = open(raw, "rb").read()

# Flatten alpha to fully transparent or fully opaque: premultiplying by 0 or 1
# loses nothing, so the associated file is recoverable exactly.
body = bytearray()
for i in range(0, width * height * 4, 4):
    r, g, b, a = rgba[i : i + 4]
    body += bytes((r, g, b, 255 if a >= 128 else 0))


def tiff(path, extra_samples):
    """A little-endian, uncompressed, single-strip RGBA file."""
    pixels = bytearray(body)
    if extra_samples == 2:
        for i in range(0, len(pixels), 4):
            if pixels[i + 3] == 0:
                pixels[i : i + 3] = b"\x00\x00\x00"

    # The header is 8 bytes, then the strip, then the directory: writing the
    # pixels first keeps every out-of-line value at a known offset behind it.
    strip = 8
    ifd = strip + len(pixels)
    entries = [
        (256, 3, 1, width),            # ImageWidth
        (257, 3, 1, height),           # ImageLength
        (258, 3, 4, None),             # BitsPerSample, four shorts, out of line
        (259, 3, 1, 1),                # Compression: none
        (262, 3, 1, 2),                # PhotometricInterpretation: RGB
        (273, 4, 1, strip),            # StripOffsets
        (277, 3, 1, 4),                # SamplesPerPixel
        (278, 3, 1, height),           # RowsPerStrip
        (279, 4, 1, len(pixels)),      # StripByteCounts
        (284, 3, 1, 1),                # PlanarConfiguration: chunky
        (338, 3, 1, extra_samples),    # ExtraSamples
    ]
    trailing = ifd + 2 + 12 * len(entries) + 4

    directory = struct.pack("<H", len(entries))
    for tag, kind, count, value in entries:
        if value is None:
            directory += struct.pack("<HHII", tag, kind, count, trailing)
        elif kind == 3:
            directory += struct.pack("<HHIHH", tag, kind, count, value, 0)
        else:
            directory += struct.pack("<HHII", tag, kind, count, value)
    directory += struct.pack("<I", 0)
    directory += struct.pack("<4H", 8, 8, 8, 8)

    open(path, "wb").write(b"II\x2a\x00" + struct.pack("<I", ifd) + pixels + directory)


tiff(f"{out}/rgba8.tiff", 1)
tiff(f"{out}/rgba8-assoc.tiff", 2)
