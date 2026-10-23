# Lane hevcexport: export decodes HEVC, VP9 and VP8 in software

Goal: when export has to decode a source (re-encode paths) and the VA-API plugin is absent,
export uses the same software seats playback uses, routed by codec, instead of refusing
HEVC, VP9 and VP8 by name. Hardware is still tried first.

Base: `codecs-integration` at `3a6e5c7` (the hevcsw merge). Branch `lane-hevcexport`, commit
`5f0f841`, worktree `~/.cache/wt/edith-hevcexport`.

## What changed

- `crates/engine/src/decode/hevc.rs`, `vp9.rs`, `vp8.rs`: each seat's decode is now a
  `Stream` that pulls one picture at a time (`Stream::open`, `Stream::next`) and narrows it
  into a reusable `I420`. The span runners (`run_hevc`, `run_vp9`, `run_vp8`) keep their
  speed, lateness and render logic and read the same streams. HEVC's pictures, landing rule,
  crop and 10-to-8-bit narrowing are unchanged; they moved from the runner into `Stream`.
- `crates/engine/src/decode.rs`: `I420` (tightly packed 8-bit planes) and `Pulled`, the
  dispatcher over the three streams (`Pulled::open` by codec, `Pulled::next`). H.264 and AV1
  return `None`: H.264 keeps its own loop, AV1 is chunked (see below).
- `crates/engine/src/export.rs`: `SwDecoder` is now a seat enum. The H.264 seat is the old
  code, moved into `H264Seat` unchanged. The pulled seat calls `Pulled`, skips pictures before
  the in point, and hands the planes to `Yuv::pixels`. The codec refusal is now only for AV1.
  Both large seat variants are boxed to keep the new enum clippy-clean.
- `crates/engine/src/demux.rs`: `needs_plugin` wording for HEVC, VP9 and VP8 no longer claims
  export lacks a software path. Only AV1 is still refused by name without the plugin.
- `crates/engine/tests/export_software.rs` (new): exports `[5, 25)` of `test_hevcsw_8.mkv`,
  `test_hevcsw_10.mkv`, `test_vp9.webm` and `test_vp8.webm` to H.264 mp4 (the span starts off
  any sync sample, so the output is re-encoded). It checks the frame count (= span length),
  the size (= source size), the duration (within one frame), and the PSNR of the first and last
  frames against ffmpeg's decode of the source at the same frames (>= 30 dB).
- `crates/engine/tests/video_copy.rs`, `tests/anamorphic.rs`: comments only. The stale
  "export is H.264-only" text was corrected. The anamorphic skip gate is unchanged.

AV1 is not routed. Its software decoder is chunk-based (`decode_stream_with`) and needs a whole
run of pictures held before any is returned, so export cannot pull it one at a time without
buffering a GOP. It stays refused by name without the plugin, as before.

## Verify (server, `scripts/remote-run.sh edith-hevcexport --with-edith`, release)

- `cargo test -p engine --release --test video_copy`: 10 passed, 1 ignored (`a_real_film`, needs
  `VE_FILM`). The three targets now pass: `a_canvas_the_source_is_not_is_encoded_at_the_canvas`,
  `a_cut_off_the_keyframe_grid_falls_back_to_the_encoder`, `a_touched_timeline_is_still_encoded`.
- `cargo test -p engine --release --test export_software`: 4 passed. First/last PSNR vs ffmpeg:
  HEVC 8-bit 46.15 / 38.11 dB; HEVC 10-bit 45.46 / 37.46 dB; VP9 50.62 / 45.19 dB; VP8 50.79 / 45.69 dB.
- `cargo test -p engine --release --no-fail-fast` (full, before the final two boxing edits):
  874 passed, 2 failed, 56 ignored. The failures:
  - `playback::import_refuses_what_does_not_match`: known.
  - `subtitles::a_project_naming_many_tracks_of_one_file_walks_it_once_and_keeps_its_order`:
    known load-flaky. Rerun alone: `cargo test -p engine --release --test subtitles`, 22 passed.
  The previous lane's run had 862 passed and 6 failed. The 3 `video_copy` failures are gone;
  `export_software` adds 4.
- After the last two boxing edits, `cargo test -p engine --release --test export_software --test video_copy`
  was rerun: 4 passed, and 10 passed with 1 ignored. These are the only tests that touch the edited
  seat code that ran after the full run.
- `cargo clippy -p engine --all-targets`: exit 0. Compared with the base log, the counts for
  `decode.rs` (12), `export.rs` (10) and `demux.rs` (6) are unchanged, so no new warnings. The new
  files `decode/hevc.rs`, `vp8.rs`, `vp9.rs` and `tests/export_software.rs` are warning-free.
  The `Pictures` size-difference warning in `export.rs` predates this lane.

## Notes and what is left

- Formatting: the repo is not rustfmt-clean. `cargo fmt` would reformat unrelated files, so
  nothing outside this lane was reformatted. My new and rewritten files pass `rustfmt --check`.
- PSNR method: an early version used ffmpeg's `psnr` filter, which paired frames by timestamp and
  reported about 24 dB on every frame. A direct per-frame comparison of ffmpeg's raw decodes showed
  the export was right. The test now compares raw frames, and that is what is committed.
- `ec-vp9` was checked sample for sample against ffmpeg's decode of `test_vp9.webm` (identical on the
  frames sampled: 0, 1, 5 and 10), so the 45 to 51 dB figures are the export's H.264 encode, not the decode.
- VP8 playback now copies each picture's planes (the stream owns an `I420` buffer). It was
  zero-copy before. Not measured.
- Not done: AV1 export through software decode (above).
- The test needs `ffmpeg` for the PSNR half. Without it, the test prints a skip and passes after
  the container checks, as `hevc_export.rs` does.
