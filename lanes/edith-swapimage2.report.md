# edith lane swapimage2: engine off the image crate

Branch `lane-swapimage2` (worktree `~/.cache/wt/edith-swapimage2`), base `codecs-integration` d4877d6.
Commit `4c1a9c6` ♻️ refactor(decode): stills read and turned by ec-image, image crate gone from engine.

Result: `engine` no longer depends on `image` at all (no normal, no dev dependency).
`image` stays only in the app, through gpui 0.2.2 (see "App" below).

## Parity before switching

Harness: temporary integration test `crates/engine/tests/zz_swap_parity.rs` (not committed; copy kept at
`~/.cache/wt/edith-swapimage2-parity-harness.rs.txt`). Old = the `image` crate exactly as `decode.rs` used it
(`ImageReader` + `into_decoder().orientation()`, `into_dimensions()`, `imageops::rotate*`, `DynamicImage::apply_orientation`).
New = `ec_image::probe` / `probe_reader`, `ec_image::transform`.

Inputs (159 files):
- every still on disk: `assets/test_still.png`, `assets/test_tone_cover.jpg`, and all 94 files in
  `edith_codecs/fixtures/stills` (png, jpg, webp, gif, bmp, tiff);
- generated EXIF fixtures, orientation 0 (tag holds 0), 1 to 8, and 9, in both byte orders, for JPEG (APP1),
  PNG (`eXIf`) and WebP (`EXIF` chunk), plus untagged JPEG/PNG/WebP.

Log: `~/.cache/wt/edith-swapimage2-parity-before.log` (`PARITY files=159 mismatches=80`).

| Check | PNG / JPEG / WebP (what `is_image` admits) |
|---|---|
| orientation, old vs `probe_reader` | 0 mismatches (all eight values, both byte orders, all three containers) |
| width/height, old vs `probe_reader` | 0 mismatches |
| `probe(slice)` vs `probe_reader` | 0 mismatches |
| `rotate90/180/270` (RGB8) on the same pixels | identical |
| `apply_orientation` 1..=8 (RGB8 and RGBA8) on the same pixels | identical |

The 80 mismatch lines are all BMP, GIF or TIFF (40 files). The engine's `image` build has only png, jpeg and webp
features, so the old path returns "not supported" there, while `ec_image` answers. Both `image_size` callers
(`playback.rs:1173`, `app/src/player/export.rs:40`) are gated by `crate::is_image`, so admitted formats are the only
ones that reach it and nothing changes for them.

## Engine functions after switching

`crates/engine/tests/zz_swap_parity.rs` (run again, against the switched `engine::image_orientation` / `image_size`):
119 files where the old path answered, 24 of them mirror refusals.
`ENGINE compared=119 mirror-refusals=24 mismatches=0` (log `~/.cache/wt/edith-swapimage2-step-b.log`).

## What changed

- `crates/engine/src/decode.rs`
  - `image_orientation` = `probe_reader(File)` then `rotation_of_exif(u8)`. The refusal for EXIF 2/4/5/7 is unchanged
    (`UnsupportedStillOrientation { exif }`).
  - `image_size` = one probe (`probe_header`), so a library row costs one bounded read.
  - `turn_rgb(rgb, w, h, steps)` = `ec_image::transform::rotate90/180/270` on `Layout::Rgb8`.
  - `Still::open` unchanged apart from the call shape; `ec_image::open` already decoded it.
  - `rotation_of_exif` doc corrected: values 0 and above 8 were always read as upright (the `image` crate's
    `NoTransforms`), not refused as the old doc said. Behaviour is unchanged.
- `crates/engine/Cargo.toml`: `image` removed from `[dependencies]`; the `[dev-dependencies]` section is gone.
- `crates/engine/tests/exif_orientation.rs`: the base JPEG was written by `image`'s JPEG encoder. It is now
  `crates/engine/tests/data/quadrants_64x32.jpg`, a 677-byte file made once with PIL (quality 90, 4:2:0, 64x32,
  the same four quadrants). The APP1 is still spliced by hand, as before. `ec-image` has no JPEG encoder, so this
  is the only way to remove the dev dependency without changing the fixture's pixels.
- `crates/engine/tests/images.rs`: the still oracle reads `assets/test_still.png` with `ec_image::open`
  (bit-exact for PNG, per the earlier swapimage report).
- Engine tests had no PNG round trip, so there was nothing to swap there.

## App

- `crates/app/src/ui/preview.rs` `save_screenshot`: the RGBA buffer after `bgra_to_rgba` is a plain RGBA buffer, so it
  is written with `ec_image::png_encode::encode_rgba8` and `std::fs::write`. Nothing else in the app changed there.
- `crates/app/src/tests/media.rs` `a_screenshot_frame_round_trips_through_its_png`: writes with the ec encoder, reads
  back with `image::open` (an independent decoder, so the round trip crosses both crates).
- `crates/app/Cargo.toml`: `ec-image` added.
- `image` stays in the app. gpui 0.2.2's `RenderImage::new` takes `image::Frame`, and the app builds
  `image::RgbaImage` for it in `player/transport.rs:236`, `player/export.rs:157` and `ui/preview.rs:457`. Those are gpui
  types, not a decode or encode, so they are left (earlier report, gap G5).
- Cost of the switch: the ec PNG encoder's output is larger than `image`'s (earlier measured: 134275 vs 93292 bytes on
  `fixtures/stills/source.png`). Screenshots are written rarely, so I judged that acceptable.

## Verify (all through `scripts/remote-run.sh edith-swapimage2 --with-edith`, host c)

- `cargo test -p engine --release --no-fail-fast`: 872 passed, 2 failed (log `~/.cache/wt/edith-swapimage2-engine-test.log`).
  - The two failures are the allowed ones: `playback::import_refuses_what_does_not_match` and the load-flaky
    `tonemap::tests::perf_4k_and_1080p`.
  - The other load-flaky tests (`bitrate::no_decoder_is_opened`, `a_caption_window_no_project_can_hold_is_refused_by_name`,
    `subtitles::a_project_naming_many_tracks_of_one_file_walks_it_once_and_keeps_its_order`) passed.
  - The pass count is 872, not the 878 the earlier swapimage report gave. I did not confirm where the difference comes
    from; the earlier run may have included a temporary test that has since been deleted.
- `cargo test -p engine --release --test exif_orientation --test images` (after the final formatting edit): 5/5 and 8/8.
- `cargo test -p edith screenshot`: 2/2 (`a_screenshot_frame_round_trips_through_its_png`,
  `a_repeated_screenshot_name_gets_a_numeric_suffix`).
- `cargo build -p edith`: exit 0.
- `cargo clippy -p engine --all-targets` and `cargo clippy -p edith --all-targets` (`--message-format short`,
  log `~/.cache/wt/edith-swapimage2-build-clippy.log`): no warnings on lines this lane touched.
  - Engine: the decode.rs warnings are at lines 49, 737, 1481, 2199, 2233, 3004, 3023, 3815, 3966, 4005, 4099,
    3087, 3285, none in the edited range 1751 to 1915.
  - App: `preview.rs:615` (`chunks_exact_mut`) sits in `bgra_to_rgba`, which this lane did not change.
  - Pre-existing, not from this lane: `edith (bin "edith" test)` fails clippy on `crates/app/src/tests/layout.rs:2073`
    (`approx_constant`, error-level), so clippy stops for that target. That file is not touched.
- `cargo tree -p engine -e normal -i image`: "package ID specification `image` did not match any packages", i.e. no
  match. `cargo tree -p engine -e all` shows no `image` either. I ran `cargo tree` locally (metadata only, no build).
- `cargo tree -p edith -e normal -i image` (main checkout, read-only): `image` is reached from `edith` (direct), `engine`
  (no longer, after this lane, but the main checkout still has the old engine), and `gpui`.

## Not done / left

- TIFF orientation is not read by ec-image (`Info::orientation` = 1 for TIFF). Not used by the engine (TIFF is not admitted).
- The probe reads at most 1 MiB, so an EXIF block that starts past 1 MiB reads as upright (documented in ec-image).
- Mirror orientations (2/4/5/7) are still refused by name in the engine, which is the existing contract.
- App keeps `image` for gpui (G5).

## Notes on the process

- The orchestration file is `edith_codecs/lanes/ORCHESTRATION-EDITH.md` (the root did not have it). Followed it.
- I briefly wrote `/tmp/x-orig.rs` for a rustfmt check and deleted it at once. Nothing else was written to `/tmp`; every
  other write went to `~/.cache`, and the harness used the remote `TMPDIR`.
