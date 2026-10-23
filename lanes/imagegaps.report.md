# imagegaps: the four `image`-crate gaps in ec-image

Goal: close the four things the edith engine still needs `image` for (see
`lanes/edith-swapimage.report.md`, gaps G1 to G4), inside `crates/ec-image` and `crates/ec-inflate`.
Base sha: 52dcce01 (main at worktree creation). Branch `lane/imagegaps`.

## What changed

1. **Header probe** (`crates/ec-image/src/probe.rs`, re-exported at the crate root).
   - `probe(prefix: &[u8]) -> Result<Info>`: reads at most `PROBE_LIMIT` (1 MiB) of the slice.
   - `probe_reader<R: Read>(reader: R) -> Result<Info>`: reads at most `PROBE_LIMIT` bytes from the
     reader (`Read::take`), allocates no more than that window.
   - Neither decodes pixels. Dimensions come from the header of each format; orientation from the
     EXIF block that lies inside the window. An EXIF block past the window reads as 1 (documented).
   - Covers JPEG (APP1 `Exif\0\0`), PNG (`eXIf`), WebP (`EXIF` chunk), GIF and BMP (orientation 1).
     TIFF is probed for size only; its orientation tag is not read (orientation 1, see "Left").
   - `tEXt`/`zTXt` "Raw profile type exif" is **not** read: `image` 0.25.10 does not read it either
     (it only reads IPTC text keys), so both agree on ignoring it.
2. **Orientation in `info()`**: `ec_image::Info` has a new field `pub orientation: u8` (1..=8, 1 when
   the file has none). `info()` now also fills it. `Metadata::orientation` (the decoded image) is
   unchanged in type and still reported, not applied.
   - `info()` on WebP walks the chunk headers without requiring the whole file (it tolerates a chunk
     that runs past the end, which the probe needs). Before, a truncated WebP was an error from
     `info()`; now it succeeds when the image header is inside the bytes it was given.
3. **Rotate and flip** (`crates/ec-image/src/transform.rs`): for `Layout::{Rgb8, Rgba8}`:
   `rotate90` (clockwise), `rotate180`, `rotate270`, `flip_horizontal`, `flip_vertical`, and
   `apply_orientation(layout, w, h, pixels, orientation) -> Result<(u32, u32, Vec<u8>)>` for all eight
   EXIF orientations. Mapping is the one `image` applies (`DynamicImage::apply_orientation`).
4. **PNG encoder** (`crates/ec-image/src/png_encode.rs`): `encode_rgb8(w, h, &[u8]) -> Result<Vec<u8>>`
   and `encode_rgba8`. 8-bit, one `IDAT`, per-row filter chosen by the minimum-sum-of-absolute-residuals
   heuristic, CRC on every chunk.
5. **Deflate encoder in ec-inflate** (`crates/ec-inflate/src/deflate.rs`, new, since ec-inflate had
   decode only): `deflate_raw(&[u8]) -> Vec<u8>`, `deflate_zlib(&[u8]) -> Vec<u8>`. Greedy LZ77,
   32 KiB window, hash chain depth 16, one fixed-Huffman block; stored blocks when they are smaller.
   Unit tests round-trip through `inflate`.
6. **EXIF helper** (`crates/ec-image/src/exif.rs`, private): one `tiff_orientation(&[u8])` that
   replaces the duplicate parser in `jpeg.rs` and `webp/mod.rs`. It reads IFD0 tag 0x0112 with type
   SHORT and count 1, value 1..=8 only.
7. **Fuzz**: `crates/ec-image/fuzz/fuzz_targets/probe.rs` (new `probe` bin in `fuzz/Cargo.toml`, added
   to `seed.sh`). It calls `probe`, `info` and `probe_reader`. The integration test
   `probing_arbitrary_mutations_never_panics` covers the same ground in the normal test run.

### The PNG eXIf disagreement: who is right

Both the PNG 1.5 eXIf definition and the EXIF 2.3 orientation table say the same thing: the `eXIf`
chunk holds a bare TIFF block (`II*\0` or `MM\0*`, no `Exif\0\0` header), and orientation is IFD0
tag 0x0112. A PNG with `eXIf` orientation 6 means "rotate 90 CW to display". **`image` was right
(6). ec-image was wrong:** it did not read `eXIf` at all and reported 1. ec-image now reads it, and
the two agree on all eight orientations, in both byte orders. The chunk is not required to carry an
`Exif\0\0` prefix, and neither reader accepts one, so a prefixed `eXIf` reads as no orientation on
both sides.

Same rule for WebP: the `EXIF` chunk holds a bare TIFF block (libwebp and `image` agree). A prefixed
WebP EXIF chunk reads as no orientation, on both sides. JPEG APP1 keeps the `Exif\0\0` header, as
the JPEG EXIF spec requires.

## Verify

All cargo runs went through `scripts/remote-run.sh imagegaps`, host c, with
`CARGO_TARGET_DIR=~/.cache/tgt-imagegaps TMPDIR=~/.cache/tmp-imagegaps` set locally. Results:

- `cargo clippy -p ec-image --all-targets -- -D warnings`: clean (exit 0).
- `cargo clippy -p ec-inflate --all-targets -- -D warnings`: clean (exit 0).
- `cargo test -p ec-image --release` (default threads): all pass.
  - lib 35/35, differential 19/19, fuzz 19/19 (59 s), fuzz_regressions 5/5, real_library 2/2.
  - gaps (new) 10/10 (see below).
- `cargo test -p ec-inflate --release`: 6/6 and 7/7 across its test binaries (the deflate unit tests are in the first).
- Fuzz: `cargo +nightly fuzz run probe -- -max_total_time=90` (seeded by `seed.sh`): 994,864 runs in 91 s, no crash or sanitizer report.
- The fuzz crate builds with `cargo check --bins` (the new `probe` bin included).

### New parity tests (`crates/ec-image/tests/gaps.rs`)

- `every_orientation_is_probed_and_applied_like_image_for_jpeg_png_and_webp`: for orientation 1..=8,
  both byte orders, JPEG (APP1), PNG (eXIf), WebP (EXIF chunk). Checks `info`, `probe`, decode meta,
  and the upright pixels against `image` (`ImageReader` orientation plus `apply_orientation`). Exact
  on PNG and WebP; JPEG within 3 counts (the existing rounding class).
- `transforms_match_image_on_random_rgba_for_all_orientations`: RGBA 9x4, all eight, exact.
- `gif_and_bmp_report_orientation_one`.
- `probe_reads_the_header_and_never_the_pixels`: PNG with a garbage IDAT still probes; `decode` fails.
- `a_prefix_of_a_large_jpeg_probes_to_the_same_answer`: 640x480 JPEG, half the file.
- `probe_reader_stops_at_the_window`: an endless reader is read exactly `PROBE_LIMIT` bytes.
- `a_huge_declared_size_is_reported_without_allocating_it`: PNG header 2^31-1 square probes, no decode.
- `png_encoder_round_trips_bit_exact_and_matches_image_decode`: RGB8 97x61 and RGBA8, decoded by
  ec-image and by `image`, bit-exact.
- `png_encoder_size_against_image_on_the_still_fixture`: see sizes below.
- `probing_arbitrary_mutations_never_panics`: byte overwrites and truncations of three seeds.

ffmpeg check: the RGB8 97x61 PNG from the encoder was decoded by the local ffmpeg 
(`ffmpeg -i enc.png -f rawvideo -pix_fmt rgb24`) and compared with the expected pixels: bit-exact.

### Encoder size against `image`

`fixtures/stills/source.png` (320x240, RGB8), re-encoded both ways:

| encoder | bytes |
|---|---|
| ec-image `png_encode::encode_rgb8` | 134275 |
| `image` `PngEncoder` (default level) | 93292 |

ec-image is 1.44x the size of `image`. The gap is the Huffman coding: ours is fixed-Huffman with a
short hash chain, `image` (miniz) uses dynamic Huffman. Output decodes bit-exact.

## Left (not done, and why)

- **TIFF orientation** is not read (`Info::orientation` is 1 for TIFF). The TIFF decoder keeps
  only its first directory's layout, so IFD0 tag 0x112 needs a small change there. Not in the
  four listed gaps.
- **Metadata orientation is still not applied** by `decode`; only `apply_orientation` applies it.
  Callers that want upright pixels call `transform::apply_orientation` themselves.
- **Probe window limit**: an `eXIf`, `EXIF` or APP1 segment, or a JPEG SOF, that starts past 1 MiB is
  not seen. Measured by construction, not by a real file.
- **PNG encoder** is 8-bit RGB/RGBA only, no palette, no 16-bit, no interlace, no ancillary chunks.
  It is not wired into the engine or app (the lane is ec-image only).
- **edith side** still uses `image` for the G1 to G5 call sites in `lanes/edith-swapimage.report.md`.
  Swapping them is the next step, in the edith tree.
- The `image` dev-dependency stays, as the oracle.
