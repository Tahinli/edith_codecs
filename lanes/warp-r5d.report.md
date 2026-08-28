# lane-warp r5d report — HANDOFF (written by orchestrator from the builder's delta; WIP committed 0004e2e)

HANDOFF: 89122b0 adjudicated PROVEN-RIGHT; HORZ_B strip mvstack query fixed (bw4=8,bh4=4,
spec-correct, keep); frame 13 unchanged — next round needs libaom inspect ground truth.

## Facts (builder's delta, verbatim)
- decode.rs:5537 — HORZ_B strip mvstack query now bw4=8,bh4=4 (reject_residual gate); compiled;
  pin STILL fails frame 13; mvs byte-identical to r5c (strip 6,-8 leafA 6,8 leafB 6,-8).
- Pin diff map, quad (1,1): strip 418/512 wrong, leafA 171/256, leafB 256/256 FULLY wrong;
  first diff (31,40); deblock bleed 6+96 cells into Q01/Q10; Q00+Q10 blocks correct
  (ref=4 mapping fine).
- DEAD END: ffmpeg export_mvs — no AVMotionVector side data for AV1; use libaom inspect.
- DECISION: 89122b0 PROVEN-RIGHT — decode_inter_block `at` is 16px units (decode.rs:4858-9
  px = c*SUB); (r32*2+1, c32*2) = (3,2) 16px = mi(12,8) matches trace; no revert.
- /tmp/libaom-build has aomdec (generic C, CONFIG_INSPECTION=1); aomdec 3.13 lacks --inspect;
  build the examples/inspect target for a ground-truth per-block mv/ref dump.
- OPEN: frame 13 — entropy matches, ALL quad-(1,1) blocks pixel-wrong: either our mvstack
  winner differs from libaom (inspect decides) or MC internals; leafB filter=[0,0] fully
  wrong is the lead.

## Dispositions
- done by orchestrator: WIP commit 0004e2e + this report (cap ate both again).
- fix-now (r5e): frame-13 root cause via libaom inspect ground truth.
- keep: bh4 fix — spec-correct regardless; may clear the NEXT mismatch once the primary
  cause is fixed.
