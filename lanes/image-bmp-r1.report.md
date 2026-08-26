# lane-image-bmp r1 — a BMP decoder for the gpui surface

`gpui` names `ImageFormat::Bmp`, so the shim needs a decoder behind it. This
lane adds `crates/ec-image/src/bmp.rs`, wires the format through `ec_image`'s
guess/decode/info dispatch, generates a fourteen-file BMP corpus and gates it
against the incumbent `image` 0.25.10.

## What the decoder reads

| shape | fixture | source |
| --- | --- | --- |
| 1-bit palette | `mono.bmp` | ImageMagick |
| 4-bit palette | `pal4.bmp` | ImageMagick |
| 8-bit palette | `pal8.bmp` | ImageMagick |
| RLE8 | `rle8.bmp` | ImageMagick |
| RLE4 | `rle4.bmp` | helper (ImageMagick's RLE switch always picks 8-bit) |
| 16-bit 5-5-5, `BI_RGB` | `rgb555.bmp` | helper |
| 16-bit 5-6-5, `BI_BITFIELDS` | `bf565.bmp` | helper |
| 24-bit | `rgb24.bmp`, `odd24.bmp` (37x29) | ImageMagick |
| 32-bit + alpha, `BITMAPV4HEADER` | `alpha32.bmp` | helper |
| 32-bit + alpha, `BITMAPV5HEADER` | `v5alpha.bmp` | ImageMagick |
| top-down rows (negative height) | `topdown24.bmp` | helper |
| 12-byte OS/2 v1 header | `os2v1.bmp` | helper |
| 16-byte OS/2 v2 header | `os2v2.bmp` | helper |
| `BI_PNG` / `BI_JPEG` wrappers | `embedded-png.bmp`, `embedded-jpeg.bmp` | helper |

The helper is `scripts/lib/gen-bmp-shapes.py`, called from
`scripts/gen-still-fixtures.sh`; it writes from the same raw RGBA dump the
ImageMagick files come from, so every fixture is the same picture.

## Gate

`cargo test -p ec-image --test differential bmp`

| fixture | max sample delta vs incumbent | corr |
| --- | --- | --- |
| alpha32.bmp | 0 | 1.000000 |
| bf565.bmp | 0 | 1.000000 |
| mono.bmp | 0 | 1.000000 |
| odd24.bmp | 0 | 1.000000 |
| os2v1.bmp | 0 | 1.000000 |
| pal4.bmp | 0 | 1.000000 |
| pal8.bmp | 0 | 1.000000 |
| rgb24.bmp | 0 | 1.000000 |
| rgb555.bmp | 0 | 1.000000 |
| rle4.bmp | 0 | 1.000000 |
| rle8.bmp | 0 | 1.000000 |
| tiny.bmp | 0 | 1.000000 |
| topdown24.bmp | 0 | 1.000000 |
| v5alpha.bmp | 0 | 1.000000 |

Fourteen of fourteen pixel-exact. BMP is a lossless container of raw samples,
so anything else would be a header read differently.

Three fixtures the incumbent cannot arbitrate, each with its own test:

- `embedded-png.bmp` / `embedded-jpeg.bmp` — `image` refuses `BI_PNG` and
  `BI_JPEG` outright. The oracle is the incumbent decoding the *payload*
  directly: max delta 0 for the PNG wrapper, 3 for the JPEG one (bar 5, the
  same bar the plain JPEG comparison uses — the IDCT is implementation-defined).
  `a_bmp_wrapping_another_format_decodes_to_that_format` also checks the
  header-only path agrees with the pixels.
- `os2v2.bmp` — `image` answers "Unknown bitmap header type (size=16)";
  ffmpeg's BMP decoder reads 16-byte headers, so the format is read here too.
  The oracle is the same picture in a v1 header:
  `an_os2_v2_header_reads_as_the_same_picture` asserts pixel equality, and
  asserts the incumbent still refuses, so the test fails loudly if that premise
  goes stale.

## Fuzz

`cargo test -p ec-image --test fuzz --release`, 10 000 mutations each from a
fixed seed:

| sweep | seed file | decoded | panics |
| --- | --- | --- | --- |
| `bmp_survives_mutation` | `tiny.bmp` | 2557 | 0 |
| `a_run_length_bmp_survives_mutation` | `rle8.bmp` | 4730 | 0 |

The run-length path gets its own sweep because it is the one that writes at a
position the *file* chooses — RLE delta codes move the cursor. A fifth
signature was added to
`arbitrary_bytes_behind_a_signature_are_refused_not_believed`: "BM" alone is
two bytes anything could start with, so `ImageFormat::guess` also requires the
file header to agree with itself (a defined info-header length, pixels after
the headers) before the bytes reach the decoder.

## Refusals

None left in `bmp.rs`. The two shapes first written as refusals — embedded
PNG/JPEG payloads and the short OS/2 v2 header — are decoded instead:
the payloads by delegating to this crate's own PNG and JPEG decoders, the
short header by reading its first 16 bytes, which are the Windows layout.
What remains rejected is corrupt input, not an absent capability: an info
header shorter than 12 bytes, a bit depth the format does not define, a
run-length compression at the wrong depth, a zero-sized picture.

## Shim

`shims/image` maps `ec_image::ImageFormat::Bmp` to its own `ImageFormat::Bmp`,
which — like `Gif` and `Tiff` — exists only behind the `gpui` feature, since
gpui is the only consumer that names it. `cargo test -p image --features gpui`
is 9/9; the default build is 4/4.

## His library

No BMP files exist under `~/Downloads`, `~/Videos` or `~/Pictures` (`find -iname
'*.bmp'` returns nothing), so there is no real-library sweep to run for this
format. Fixture-verified only, and that is the whole population.

## Left

TIFF, the third format `gpui` names, is not started.
