# edith lane swapimage: still-image decode through ec-image

Branch `lane-swapimage` (worktree `~/.cache/wt/edith-swapimage`), base `codecs-integration` 97673ee.
Commit `4289c42` ♻️ refactor(decode): still pictures decoded by ec-image.

Result: the still-picture **decode** path is on ec-image. The `image` crate is NOT removed:
the engine still uses it for three header/pixel helpers ec-image cannot do, and the app needs it
for gpui's `RenderImage` type. `cargo tree` below shows both.

## 1. Every `image::` use in the edith workspace

| Site | What it does | Crate | Disposition |
|---|---|---|---|
| `engine/src/decode.rs` `Still::open` (~l.1817) | decode a placed still to RGB8 planes | engine | **swapped** to `ec_image::open` + `to_rgb8()` |
| `engine/src/decode.rs` `image_orientation` (l.1785) | EXIF orientation from the header alone (library rows, `Still::open`) | engine | kept on `image` (gap G1) |
| `engine/src/decode.rs` `image_size` (l.1895) | picture size from the header alone (library rows) | engine | kept on `image` (gap G2) |
| `engine/src/decode.rs` `turn_rgb` (l.1874) | quarter-turn of a turned still (`imageops::rotate90/180/270`) | engine | kept on `image` (gap G3) |
| `engine/src/decode.rs` `rotation_of_exif` (l.1758) | maps `image::metadata::Orientation` to `Rotation` | engine | unchanged, used by `image_orientation` |
| `engine/tests/images.rs:48` | test oracle: `image::open(test_still.png).to_rgb8()` | engine test | kept (oracle must stay independent) |
| `engine/tests/exif_orientation.rs:91-107` | encodes the test JPEG with EXIF | engine test | kept (ec-image has no encoder, G4) |
| `app/src/player/export.rs:157-159` | `RgbaImage::from_raw` + `Frame` for gpui `RenderImage` | app | kept (gpui type, G5) |
| `app/src/player/transport.rs:236-244` | same, for the playback frame | app | kept (G5) |
| `app/src/ui/preview.rs:457-458` | same, for PGS subtitle bitmap | app | kept (G5) |
| `app/src/ui/preview.rs:612-641` | `RgbaImage::save` writes a screenshot PNG | app | kept (encoder, G4) |
| `app/src/tests/media.rs:1658-1660` | PNG write/read round trip for screenshot test | app test | kept |

No other crate in the workspace uses `image`. `engine-hw` and `engine-audio` have no hits.

## 2. ec-image API vs. edith needs

ec-image (`crates/ec-image/src/lib.rs`) provides: `decode`, `decode_with_limits`, `info`,
`open`, `open_info`, `decode_animation`, `Image` with `to_rgb8/to_rgba8/to_rgb16/to_rgba16/to_video_frame`,
`Metadata::orientation` (JPEG APP1 and WebP EXIF only), and PNG/JPEG/WebP/GIF/BMP/TIFF decoders.
It has **no encoder, no resize, no rotate/flip, and no reader-based header probe**.

Mapping for the one swapped site:
- `image::ImageReader::open(..).with_guessed_format().into_decoder().orientation()` → kept on `image`
  (see G1) and called from `Still::open` as before.
- `image::DynamicImage::from_decoder(decoder).to_rgb8()` → `ec_image::open(path)?.to_rgb8()`.
- `image::imageops::rotate90/180/270` on `RgbImage` → unchanged (G3), now fed via `RgbImage::from_raw`
  only when the still is turned, so an upright still never touches `image` for pixels.

## 3. Gaps (not swapped; ec-image was not edited)

- **G1 `image_orientation` (header-only EXIF orientation).** ec-image's `info()` returns only
  dimensions and format. Getting orientation needs `decode()`, which decodes all pixels, so library
  rows would turn from a header read into a full decode. Also ec-image does not read PNG `eXIf`
  chunks at all (measured below), while `image` does, so the two paths would disagree on a
  phone-exported PNG. `Still::open` therefore takes its turn from `image_orientation`, not from
  `ec_image::Metadata::orientation`.
- **G2 `image_size`.** `ec_image::info` / `open_info` take the whole file in memory (no reader
  form). `image`'s `into_dimensions` reads only the header. Swapping would read every library
  picture in full on every row. Kept.
- **G3 quarter turns.** ec-image has no pixel transforms. A ~15-line rotate helper in edith would
  remove this, but the lane instruction was to leave gaps on `image`, so it is left.
- **G4 encoders.** ec-image has no encoder. Screenshot PNG (`app/src/ui/preview.rs` `save_screenshot`)
  and the engine's JPEG test fixture stay on `image`.
- **G5 gpui types.** gpui 0.2.2's `RenderImage::new` takes `image::Frame`, and gpui depends on
  `image` directly (`gpui/src/platform.rs` decodes GIF/PNG/JPEG/WebP itself). The app's `image`
  dependency therefore cannot go while gpui 0.2.2 is in use, whatever edith does. The app crate is
  **not** changed in this lane.
- **PNG eXIf (G1 detail).** Confirmed by the parity run below: ec-image reports orientation 1 for a
  PNG carrying `eXIf` orientation 6, while `image` reports 6.

## 4. Parity (before switching)

Harness: a temporary integration test (`crates/engine/tests/zz_image_parity.rs`, deleted, not
committed) decoding each input through `image` (`ImageReader` → `DynamicImage::from_decoder` →
`to_rgb8`/`to_rgba8`, the old `Still::open` path) and `ec_image::decode` (`to_rgb8`/`to_rgba8`).
Output: `~/.cache/wt/edith-swapimage-parity2.log`.

Edith test fixtures (`assets/`):

| Input | Dims (image / ec) | RGB8 differing bytes | Note |
|---|---|---|---|
| `test_still.png` 640x360 | 640x360 / 640x360 | 0 of 691200 | exact |
| `test_tone_cover.jpg` 64x64 | 64x64 / 64x64 | 0 | exact |

Generated inputs covering the formats the engine admits (the tests generate them with `image`'s encoders):

| Input | Dims | RGB8 differing | Max count | Avg count |
|---|---|---|---|---|
| PNG rgb8, rgba8, l8, la8 (97x61) | same | 0 | 0 | 0 |
| PNG rgb16, rgba16, l16 (97x61) | same | 0 | 0 | 0 |
| JPEG rgb8 q90, q40 (97x61) | same | 789, 624 | 3 | 1.2 |
| JPEG l8 q90 (97x61) | same | 543 | 1 | 1.0 |
| JPEG rgb8 640x480 q85 | same | 42754 of 921600 | 3 | 1.19 |
| JPEG 1x1 | same | 0 | 0 | 0 |
| JPEG rgb8 with EXIF 6 / 2 (mirror) / 8 | same | 784 / 794 / 722 | 3 | ~1.2 |
| WebP lossless rgb8, rgba8 | same | 0 | 0 | 0 |
| WebP rgb8 with EXIF 6 (VP8X+EXIF chunk) | same | 0 | 0 | 0 |
| PNG rgb8 with eXIf orientation 6 | same | 0 | 0 | 0 |

Explanation of differences: all JPEG differences are at most 3 of 255 counts, which is the
decoder's IDCT and chroma-upsampling rounding (ec-image's own differential test records the same
class of difference, "within rounding"). PNG and lossless WebP are bit-exact, including 16-bit
samples and the `narrow` rounding. EXIF orientation values agree for JPEG and WebP, and the
mirror case 2 is read identically on both sides. The one real divergence is the PNG `eXIf`
orientation (G1), which is why orientation stays on `image`.

Consequence for the still-suite oracle: `images.rs::close` allows 6 counts, which covers the
±3 JPEG drift; the fixture still is PNG and exact.

## 5. Verify

Engine test run (release, `--no-fail-fast`), via remote-run host c, log
`~/.cache/wt/edith-swapimage-engine-test1.log`:

```
EDITH_TREE=$HOME/.cache/wt/edith-swapimage scripts/remote-run.sh edith-swapimage --with-edith -- cargo test -p engine --release --no-fail-fast
```

- 878 passed, 1 failed, 56 ignored (summed over the test binaries).
- The one failure is `playback::import_refuses_what_does_not_match` (known, allowed).
- `tests/images.rs` (still suite, including `a_still_trims_out_to_its_cap_and_no_further`) passes.
- `tests/exif_orientation.rs`: 5 passed.
- The load-flaky tests (`tonemap::perf_4k_and_1080p`, `bitrate::no_decoder_is_opened`,
  `a_caption_window_no_project_can_hold_is_refused_by_name`,
  `subtitles::a_project_naming_many_tracks_of_one_file_walks_it_once_and_keeps_its_order`) all passed in this run.

Clippy: `cargo clippy -p engine --all-targets` via remote-run, log `~/.cache/wt/edith-swapimage-clippy.log`.
169 warning/error lines in total (pre-existing ones, including the `ec-av1` path dependency).
None point at `crates/engine/src/decode.rs` lines 1817–1840 (the changed block) or at `Cargo.toml`.
I did not run a clippy baseline on `codecs-integration` to diff counts, so "no new warnings" is
established by line location, not by a count comparison.

App crate: no source changed, so no app tests were run.

Dependency tree (local, `cargo tree -e normal -i image`):
- `-p engine`: `image v0.25.10` ← `engine` (still direct: G1, G3). Not removed.
- `-p edith`: `image v0.25.10` ← `edith` (direct, G5), `engine`, `gpui v0.2.2`.

`Cargo.lock` gained `ec-image` (path dep, `ec-core` and `ec-inflate` already present).

## 6. What is left

- `image` stays a normal dependency of `engine` (G1, G2, G3) and of `edith` (G4/G5 and gpui). Removing
  it needs ec-image to gain: a header-only probe that reports EXIF orientation, a reader-based info,
  a rotate/flip helper, and an encoder (PNG). Removing the app dependency also needs gpui to move
  off `image::Frame`.
- Test oracles (`images.rs`, `exif_orientation.rs`, `media.rs` round trip) still use `image`.
- Behaviour note: `ec_image::open` admits BMP/TIFF/GIF too, but `crate::is_image` still admits only
  png/jpg/jpeg/webp, so routing is unchanged.
