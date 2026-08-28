# lane-warp r5 report — HANDOFF at tool cap (written by orchestrator from the builder's delta; state committed 876b0b1)

HANDOFF: extended partitions implemented, pin advanced from refusal to a frame-13 pixel mismatch.

## (a) vs (b) determination: (b) — genuine capability gap
The refusal was real content: the pinned aomenc stream uses extended inter partition
types. HORZ_B, VERT_A, VERT_B implemented in the inter tile walk (decode.rs, +201 lines).
EVIDENCE: pin `pinned_warp_stream_decodes_pixel_exact` no longer refuses; decodes 24/24
frames; orchestrator-rerun confirms fail moved to stream.rs:3501 `frame 13 luma vs ffmpeg`
(/tmp/claude-1000/warp-r5-pinverify.log).

## Remaining defect (next round)
- Frame 13 luma mismatch, isolated to quadrant (1,1) = (32..64)x(32..64), INTRA path:
  /tmp/warp_r5_trace.txt shows only 10 single-ref EC_TRACE blocks total, all in frames 1-2;
  the 3 warp blocks are all in frames 1-2 which PASS. Frames 3+ print key-walk partition
  traces only — so frame 13 is either a key frame or inter-all-intra; which walk prints
  "TRACE partition" was still open at the cap.
- Warp-geometry hypothesis for frame 2 is DEAD (frame 2 passes).

## Resume steps (from the builder's dying words)
1. `grep -n "TRACE partition" crates/ec-av1/src/decode.rs` — key walk ~3369 vs inter ~7304;
   tells whether frame 13 is a key frame.
2. Instrument frame 13's failing quadrant: key frame → per-block intra trace (mode/ctx + px
   stats) for (32..64)x(32..64); inter-all-intra → mirror the 5801 EC_TRACE into the
   is_inter=0 branch at decode.rs:4923.
3. Root-cause quadrant-(1,1) luma diff, fix, pin twice green, then full
   `cargo test -p ec-av1 --release --lib`.

## Dispositions
- fix-now (next round r5b): frame-13 quadrant mismatch.
- done by orchestrator: WIP commit 876b0b1 + this report (builder's cap ate both).
- deferred(pin green): full lib-suite run.
