# Lane hevcsw: HEVC software decode through ec-h265

Goal (DONE.md, HEVC track, edith side): HEVC plays in software through `ec-h265` when the
VA-API plugin is absent or does not take the file, as VP9 and VP8 already do.

Base: branch `lane-hevcsw` at `82d0358` (the lane branch HEAD the changes sit on; it is the
VP8 merge, not the `98f6c72` the lane was first cut from). The changes are committed on top of it.

## What changed

- `crates/engine/src/decode/hevc.rs` (new): the HEVC software seat, `run_hevc`, in the shape of
  `run_vp9`. Feeds each demuxed access unit to `ec-h265` (`decode_annex_b`), drains pictures in
  display order, flushes at end of stream. A span starts on the sync sample at or before the
  target. Pictures output before the landing picture (decode-order index 0) are dropped: they are
  leading pictures of the landing IRAP and sit before any span target on the timeline. RASL pictures
  of a CRA landed on are skipped by the decoder itself (NoRaslOutputFlag). Each picture is cropped
  to its SPS conformance window and narrowed to 8-bit I420 (10-bit downshifts by two, as the AV1
  software seat does). Unit tests: crop geometry, and plane-for-plane equality with ffmpeg's decode
  for six fixtures (8-bit mp4/mkv, Main 10 mp4/mkv, odd 350x286, open-GOP CRA).
- `crates/engine/src/decode.rs`: `mod hevc` + dispatch arm `Codec::Hevc` on the software seat.
  Removed the HEVC-only plugin probe in `open`, and the `Gap` fallback arm (no codec reaches it).
  `probe` returns `Software` when the plugin does not take the file. The gap test became the
  positive case (`an_hevc_span_with_no_hardware_takes_the_software_seat`).
- `crates/engine/src/demux.rs`: `needs_plugin` gets a HEVC arm, worded like VP9's (playback decodes
  in software; the software export path does not). `parse_hvcc` refuses by name, at open, anything
  but 4:2:0 at one depth (4:0:0, 4:2:2, 4:4:4, mixed chroma depth) and RExt-and-up profiles (>= 4),
  in the words `av1_refusal` uses. The hvcC unit test gets a valid 4:2:0 chroma byte and the new
  refusal assertions.
- Tests updated to the new truth (not deleted): `tests/hevc.rs` (`the_software_path_opens_hevc`,
  timeline test now refuses 4:2:2 and joins HEVC), `tests/backends.rs` (probe answers hardware or
  software), `tests/hevc_hvc1.rs` (hvc1 opens on the software seat), `tests/vp9.rs` (timeline test and
  the refused-span test now use 4:2:2 HEVC as the refused clip), `tests/anamorphic.rs` (skip keys on
  the plugin being present through `engine::hw::caps()`, since `pin_software` pins `VE_SW` for the
  whole binary), `tests/hw_decode.rs` and `tests/video_copy.rs` (comments only).
- `tests/hevc_sw.rs` (new): demux of mp4/mkv at 8 and 10 bit, probe with the plugin forced off,
  full decode of all six fixtures with frame order, size and non-flat frame 0, seeks at 0, 1, 14, 15,
  16 and the last frame equal to a linear decode, 4:2:2 refused by name at open (container and
  software seat), and 1080p timing.
- `scripts/gen_fixtures.sh`: the new HEVC fixtures (`test_hevcsw_{8,10,odd,cra,422,1080}.*`).
  Generated into the main edith `assets/` (gitignored, shared).

## Verify (server, `scripts/remote-run.sh edith-hevcsw --with-edith`, release)

- `cargo test -p engine --release --lib hevc`: 5 passed, 0 failed.
- `--test hevc_sw`: 5 passed (decode and seek over all fixtures included).
- `--test hevc`: 4 passed, 3 ignored (need the plugin).
- `--test hevc_hvc1`: 2 passed, 2 ignored.
- `--test backends`: 8 passed. `--test anamorphic`: 5 passed (plugin-only tests skip on this server).
- `--test vp9`: 7 passed, 2 ignored. (Two tests fixed; both pass now.)
- `--test video_copy`: 7 passed, 3 failed (see below).
- Full `cargo test -p engine --release --no-fail-fast`, run before the final vp9 fix: 862 passed,
  6 failed: `playback::import_refuses_what_does_not_match` (known), 3 `video_copy` (below), and
  2 `vp9` (fixed since, verified above).
- `cargo clippy -p engine --all-targets`: exit 0. The new `decode/hevc.rs` lint was fixed
  (`as_chunks`). Clippy warnings reported in `demux.rs` and `decode.rs` point at code outside the lane's
  changes (the mp4 display-matrix and `open_visualizer` paths); no warning points at `decode/hevc.rs` or
  `tests/hevc_sw.rs` in the final log. Clippy is not run with `-D warnings`, so the exit code is 0.

### video_copy: 9 known failures, now 3

The 9 video_copy tests that failed on the plugin before: 6 now pass, 3 still fail. The exact
passing names were not captured per test in the final log (only the summary count, 7 passed), so
that list is not given here.

Still failing: `a_canvas_the_source_is_not_is_encoded_at_the_canvas`,
`a_cut_off_the_keyframe_grid_falls_back_to_the_encoder`, `a_touched_timeline_is_still_encoded`. These
export an HEVC source, and the export path still refuses it with "HEVC needs the VA-API plugin". The
export decoder is H.264-only (`export::SwDecoder`), and the brief scopes this lane to playback. Making
export read HEVC in software is the next step.

## Decode time, 1080p 8-bit (60 frames, server, release)

- `ec-h265` alone (decode, no conversion): 38.72 ms/frame.
- Software seat end to end (decode, crop, narrow, BGRA): 41.12 ms/frame.

## Left / notes

- HDR: `ec-h265` exposes mastering-display and content-light metadata per `Frame`. No seat consumes
  per-picture SEI, the hardware seat included. Tone mapping gets HDR from `Demuxer::light()`, which
  already reads the container tags and the first access unit's SEI, so nothing new was needed.
  Nothing was implemented in ec-h265.
- Leading pictures presented before a stream's first IRAP (IDR_W_RADL or a CRA first in the file)
  are not shown: the span cannot assign them indices. The fixtures (closed GOP IDRs, and open-GOP
  CRAs after the first IDR) do not hit this.
- Multi-layer (nuh_layer_id > 0) is not refused at open: hvcC does not carry it. The decoder ignores
  non-base layers, so the base layer plays.
- 10-bit output is narrowed to 8-bit I420 for the renderer, as the AV1 software seat does. The
  hardware seat's P010 path is not affected.
- Lane worktree note: a `assets` symlink in the worktree was copied to the server by remote-run's rsync
  and broke the sync, so it was removed; the server takes the main checkout's `assets/`.
- Not done: export of HEVC through software decode (above); `ec-h265` SCC, RExt tools and inter
  streams with known inexact pictures (from the decoder's own report) are outside this lane.
