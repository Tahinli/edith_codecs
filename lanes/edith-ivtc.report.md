# Inverse telecine (lane-ivtc) report

Branch `lane-ivtc`, worktree `~/.cache/wt/edith-ivtc`, local commits only.

## Commits
- `2b0cb573` engine: inverse telecine for hard 3:2 (H.264 + MPEG-2), soft pulldown rate in transport streams (also commits the Cargo.lock catch-up, `ec-riff -> ec-dv`)
- `1949b39f` clippy fixes (type alias in ivtc.rs; one pre-existing `div_ceil` lint in demux.rs that the server toolchain flags)
- `ff7ef3f7` mcp: refusal text keeps its old wording (an mcp test asserts it) and names the new modes

## Design
- Setting: two new `Deinterlace` variants, stored/serialised exactly like the existing mode (`deint` lines via `name()`, MCP `deinterlace {mode}`): `ivtc-auto` (Auto) and `ivtc` (On). Off = `frame`/`field`/`off` as before.
- Default stays Off (`Frame`). Auto is an opt-in: it is a heuristic over the first 60 pictures, one cadence for the whole file.
- `crates/engine/src/ivtc.rs` (new): `Cadence {phase, order}`, pure arithmetic 5 in -> 4 out (`input_of`, `output_of`, `out_count`), field match + decimation `Stage` (weave second field of the first mixed frame with the first field of the next; drop the second), detection (`detect`, `cadence_for`, cached per file; comb metric, tests all 5 phases x 2 field orders; Auto needs >=4 informative blocks and >=90% of them to pass, On takes the best), soft-flag scanners `Repeats` (MPEG-2 `repeat_first_field`, H.264 pic_timing SEI `pic_struct` 5/6, parsed by the engine itself with ec-h264-syntax).
- `Deinterlacer::cadenced(mode, demuxer)` installs the stage; `effective()` resolves `ivtc-auto`/`ivtc` to `Ivtc` or `Frame` per file; `counted_rate` / `counted_frames` / `file_frame` replace the former Field-only special cases (rate x4/5, frame count, stabilise map). Works in the H.264 playback worker, the MPEG-2 seat (`decode/mpeg2.rs`), reverse playback block starts, export (`H264Seat`, `Pulled`, layer feeds, dissolve B side) and `compose_at`.
- Soft pulldown: `TsDemuxer::open` scans the first pictures for the flags; when ~half ask for a third field and the timestamp-derived rate is >25 fps, the stream is reported at 4/5 of it (23.976), with the frame count recomputed and `Demuxer::soft_pulldown()` true. This is independent of the clip's mode (it is the stream's true rate, like a container fps).
- Timeline: put the project on 23.976 (`set_frame_rate`) for one timeline frame per film frame; the 75-picture clip is then 60 frames long. Seeks land through `Cadence::input_of`, so they equal a sequential decode.

## Codec crates (nothing edited in edith_codecs)
- ec-mpeg2 exposes `rff`/`tff`/`progressive_frame` (`PictureInfo`, `last_picture_info`), but the engine reads them straight from the picture coding extension bytes (no decode needed).
- ec-h264 does **not** parse the picture timing SEI (`pic_struct`) at all (SEI is skipped); missing: a `pic_struct` (and ideally clock timestamps) field on the decoded frame. The engine works around it by parsing the SEI from the access unit itself with `ec-h264-syntax` (SPS VUI `pic_struct_present_flag`, HRD delay lengths).

## Peer-file hunks
- `crates/engine/src/project.rs`: 2 hunks: `Deinterlace` enum gains `IvtcAuto`, `Ivtc` (+`ALL` now 5, `name()` arms `ivtc-auto`, `ivtc`).
- `crates/engine/src/playback.rs`: 3 hunks: (1) `start_span` span rate uses `counted_rate(deint, rate)` instead of the Field-only match; (2)+(3) `open_dissolve_b` computes `b_deint` first (Ivtc is kept for B, Field -> Frame as before) and applies `counted_rate` to B's start/end frames.
- No change in `crates/app` (the clip card's mode list still shows only Frame/Field/Off; adding the two entries is the peer's call).
- Also touched: `crates/mcp/src/tools/edit_fx.rs` (message), `MCP.md` (one sentence).

## Tests (`crates/engine/tests/ivtc.rs`, synthetic, ffmpeg testsrc2)
Hard telecine MPEG-2 TS (worst film frame 47.4 dB vs the original), MBAFF H.264 tff and bff (54.6 dB), 75 pictures -> 60 frames with tags 0..59; Auto locks on telecine and does not on true 59.94 interlaced (H.264 and MPEG-2), `effective(Frame)` unchanged; seeks to every 3rd film frame equal the sequential decode bit for bit (both codecs); a session on 23.976 gives `compose_at` and an export of 60 frames with pts k*1001/24000 matching the originals (>28 dB); soft MPEG-2 (patched rff/tff/progressive_frame, 3:2 pts) and soft H.264 (patched pic_struct, 3:2 pts) -> 23.976, 48 frames, seeks equal sequential decode, equal to ffmpeg's decode. 2 unit tests in `ivtc.rs`.
Note: libx264 `-x264-params pulldown=32` is silently ignored through ffmpeg (pulldown is a CLI-only x264 filter), so the soft H.264 clip is made by patching `pic_struct` into x264's `pic-struct=1` SEI.

## Gate (server, release)
- `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `cargo test --workspace --release --no-fail-fast`: 131 suites, 3528 passed, 3 failed: the two known `hw_midgop_seek` tests (VA-API) and `edith_mcp lane_name_and_deinterlace_ops` (my refusal-text change, fixed in `ff7ef3f7`, passes locally). The new ivtc suite: 6/6.

## Limits
- One cadence per file from the first 60 pictures: an edit that breaks the cadence is not followed (frames stay locked to the first phase); a static opening yields no lock in Auto.
- First use of Auto/On on a clip decodes 60 pictures synchronously (cached per path afterwards).
- Only software-decoded interlaced H.264 and MPEG-2 (TS/PS/AVI-in-TS reader) have a cadence; DV/ProRes/MJPEG interlaced are untouched.
- Soft pulldown: the reader fix is for transport/program streams (pts-derived rate). mp4/mkv soft-flagged files are presented as their container times them (the coded pictures). `Off` does not reintroduce the repeated fields for soft streams (the engine is frame-locked).
- Proxy stand-ins: a cached proxy is progressive, so Ivtc resolves to Frame there.
- Not run: GPU/hw suites beyond the known failures.
