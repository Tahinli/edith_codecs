# Lane deintmode report (branch lane-deintmode, one commit on 3feda907, not pushed)

## What changed
- deinterlace.rs: `filter_field(prev,cur,next,keep,second,threads)` (bwdif send_field: second field uses cur/next as prev2/next2); `filter_frame` is `filter_field(.., false)` so Frame is byte-identical. `Deinterlacer::with_mode(mode, interlaced, first_wanted, order)`, `push_all`/`finish_all` (two pictures per frame in Field mode, tags = field index 2t, 2t+1), `landing(start)`, `is_interlaced`, `is_fields`. `effective(mode, path)` (Frame for non-interlaced-H.264, cached), `fields_rate(rate)` (rate doubled via `Rate::then`).
- decode.rs: `Pass`/`SpanCmd` carry the mode; `run()` uses the stage; new `open_worker_with`, `open_worker_deferred_with`, `reseek_with`, `open_reversed_with` (old fns kept as wrappers, `#[allow(dead_code)]` for after the playback hunk). In Field mode every frame number (start, end, `Frame::index`, reverse top/count, range cap = 2x frame_count) is a FIELD number. Reverse doubles the sync frames, so fields play in reverse temporal order. Off = pass-through (plain decode).
- Convention: for a Field clip of an interlaced file the file's rate against the timeline is doubled; the existing map then does everything (25p timeline: field 2t = first field of frame t; 50p timeline of 25i: every field; speed/reverse/ramp go through the same Span maths).
- Stabilise: file_range keeps frame rate (analysis per file frame); a Field clip's warp keeps the FRAME's correction (`want / 2`) for both fields. Not covered by a runtime test.
- Dissolve B side (export and playback) never counts fields (Field acts as Frame, Off honoured), as it takes one picture per timeline frame.
- Interlaced streams are never parked (Off included).

## Peer-owned: playback.rs diff REQUIRED (not applied)
Without it playback/scrub ignore Field (decode layer and export/compose_at are done). Diff: /home/user/.cache/wt/edith-deintmode.playback.diff (134 lines; start_span doubles `span_rate` for Field via new `deint_at`, passes `deint` to reseek_with / open_worker_deferred_with / open_reversed_with, B honours Off). Verified: applied temporarily, it compiled and the ignored test `playback_and_scrub_show_the_field_the_export_writes` passed (viewer frame == compose_at at 8 timeline frames incl. mid-GOP); then reverted. Apply with `git apply`, then run `cargo test -p engine --test deinterlace_mode -- --ignored`. project.rs needs nothing.

## export.rs hunks (47 small hunks, all deinterlace)
use Deinterlace; LayerFeed gets `deint` (rate doubled after file_range, `want/2` for the warp, open_with/reversed); main span in `run` (deint + doubled rate after `rates[source]`, open_with/reversed); dissolve B (`b_deint`); compose_at (doubled rate, `picture_at(.., deint)`); new `span_deint`, `frame_counting`; `Reversed.deint`; `ClipDecoder::open_with` (open delegates), `reversed(.., deint)` with doubled sync frames; `decode_stream`/`Pictures::software`/`SwDecoder::open/new`/`H264Seat::open` take the mode; `H264Seat.pending` queue + `advance` drains it; parkable/flushed use `is_interlaced`. `git diff HEAD~1 -- crates/engine/src/export.rs` lists all.

## Numbers (352x288, crf 10, x264 MBAFF, 30 frames -> 60 fields)
PSNR dB, vs progressive original / ours vs ffmpeg `bwdif=mode=send_field`:
- 8-bit tff: ours 33.68, bwdif 33.67; ours vs bwdif 56.27 (first fields 90.8, second 94.7)
- 8-bit bff mkv: 33.68 / 33.67; 56.48
- High10 tff: 33.71 / 33.70; 56.27
- 4:2:2 bff: 36.98 / 37.00; 57.93
- 50p export of 25i Field clip: 60 frames, 1.200 s, 32.90 dB (tff) 32.91 (bff) after lossy re-encode vs 33.67 for bwdif; every picture differs from the next and is closest to its own moment; reverse export mirrors forward (>28 dB per picture); speed 2 gives 30 frames = every first field.
- 25p timeline Field export is byte-identical to the Frame export; Off keeps combing 0.0141/0.0118 vs Frame 0.0007/0.0009.
- Stage: first field of every frame == Frame-mode output exactly. Off == ffmpeg plain decode, exact (4 formats). compose_at: 25p Field == Frame; 50p frame 2n == Frame frame n, 2n+1 differs. Progressive source: Field/Off exports and stills byte-identical to Frame at 25p and 50p timelines.
- Session level (deinterlace/session_tests.rs): 60 field-numbered frames, 17 mid-GOP field seeks (odd and even) and a range equal sequential, reverse of 3 windows equals forward fields reversed, Off == plain Render of woven pictures, progressive unchanged. tff and bff.

Tests: crates/engine/tests/deinterlace_mode.rs (6 + 1 ignored pending playback hunk), crates/engine/src/deinterlace/session_tests.rs (4). Existing interlaced tests untouched and passing.

## Gates (host c)
- clippy -p engine -p edith_mcp -p edith --release --all-targets -D warnings: clean.
- cargo test -p engine -p edith_mcp -p edith --release --no-fail-fast: all pass except the 2 known VA-API failures (h264/hevc_mid_gop_seeks_match_a_sequential_decode).
- Note: host c disk was full; I deleted stale cargo target caches (~/.cache/tgt-* untouched since Oct 16-18, 13 dirs).
