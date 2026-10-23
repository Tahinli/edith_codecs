# edith lane av1420: refuse non-4:2:0 AV1 at open

## Goal
edith decodes AV1 only as 8/10-bit 4:2:0 (the software seat narrows to I420). 4:4:4,
4:2:2 and 12-bit streams were not checked and would be mis-indexed. Refuse them at open
with a named reason, on both the software and hardware seats.

## Base
- edith main: b7d8ed0 (lane branch `lane-av1420`, worktree `~/.cache/wt/edith-av1420`)
- Commit: 19378f7 `🐛 fix(demux): refuse AV1 that is not 8/10-bit 4:2:0 at open, by name`

## What changed
- `crates/engine/src/demux.rs`: `parse_av1c` reads the chroma format from the av1C flags
  it already parses (byte 2: monochrome, chroma_subsampling_x, chroma_subsampling_y).
  - 4:4:4 (profile 1) -> "AV1 4:4:4 is not supported — edith decodes 8/10-bit 4:2:0 AV1 only"
  - 4:2:2 (profile 2, 10-bit) -> "AV1 4:2:2 ..."; x=0,y=1 -> "AV1 4:4:0 ..."
  - 12-bit -> "AV1 12-bit ..." (same wording, same helper `av1_refusal`)
  - Monochrome is accepted (see below). Unit test covers every flag combination.
  - Both demuxers (mp4 `av1C` and mkv `CodecPrivate`) call `parse_av1c` at open, and both
    `Demuxer::open` and `DecodeSession::open` (the path the hardware seat opens through) go
    through it, so the check covers software and hardware seats with one refusal.
- `crates/engine/src/decode.rs`: comment on `narrow_av1` updated (no code change).
- `crates/engine/tests/av1.rs`: three new tests:
  - `a_non_420_av1_file_is_refused_at_open_by_name`: 444, 422, 12bit in mkv and mp4 by
    `Demuxer::open`, plus 444 and 422 by `DecodeSession::open`.
  - `a_420_av1_file_in_either_container_opens_and_decodes`: 420 and 420_10 in mkv and mp4,
    depth read right, 3 frames decoded on the software seat.
  - `a_monochrome_av1_file_opens_and_decodes`: gray in mkv and mp4, 3 frames decoded.
- `scripts/gen_fixtures.sh`: ffmpeg `libaom-av1 -cpu-used 8 -g 30`, 64x64, 3 frames,
  mkv and mp4, for yuv420p, yuv420p10le, yuv444p, yuv422p, yuv420p12le, gray.

## Monochrome decision
Allowed. ec-av1 has its own pixel-exact monochrome witness against libaom
(`a_real_libaom_monochrome_key_frame_decodes_pixel_exact`, ec-av1 stream.rs). edith's
decode of `test_av1_gray.{mkv,mp4}` delivers all 3 frames, and the worker-side I420
conversion would fail loudly on mis-sized planes. Chroma values were not compared pixel for
pixel against ffmpeg in edith itself.

## Verify
Run via `EDITH_TREE=~/.cache/wt/edith-av1420 scripts/remote-run.sh edith-av1420 --with-edith -- ...`
(host a/b, both hosts had the assets uploaded to `~/lanes/edith-av1420/edith/assets`).

1. `cargo test -p engine --release --no-fail-fast` (final tree):
   - 825 passed, 10 failed, 56 ignored (ignored = needs libengine_hw / VA-API plugin or
     real_library media).
   - AV1 binary (`--test av1`): 15 passed, 0 failed, 7 ignored. New tests pass.
   - The 10 failures are pre-existing on edith main (checked on a clean `git archive main`
     export, same assets, same server):
     - `playback::import_refuses_what_does_not_match` (1): audio import into a silent
       timeline is accepted instead of refused (`playback.rs:1227`).
     - `video_copy` (9): "HEVC needs the VA-API plugin ... there is no software HEVC decoder"
       on the server, which has no plugin.
   - `tonemap::tests::perf_4k_and_1080p` failed once under load (40 ms/frame against a
     25 ms budget) and passed on the final run and when run alone. Timing noise, not this change.
2. `cargo clippy -p engine --all-targets` (message-format short), normalised by warning text:
   main and lane are identical, 114 warnings each, none new. Edited lines are clippy-clean.
3. `rustfmt --check`: my added hunks are clean. `demux.rs` and `av1.rs` have pre-existing diffs
   that are not from this change.

## Not done / left
- A stream with an empty av1C (sequence header only in-band, the case parse_av1c takes as
  8-bit) is not format-checked at open. The mp4 and mkv writers here always write av1C, and the
  task's fallback (parse the first sequence header OBU) would need a read of the first sample at
  open. The seat-level narrowing does not catch this either. It is a known gap.
- The hardware seat was not run: the server has no VA-API plugin. The refusal is at open, before
  any seat is chosen, so it is shared by both seats, but no hardware-seat run was done.
- Commit trailer: the lane doc asks for `Co-Authored-By: Claude Opus 5.5`. The session's
  attribution rule names Haiku 5.5. The commit uses the lane doc's trailer. Change it if the
  attribution should be the other model.
- Pre-existing failures above (playback import, video_copy) are not part of this lane.
