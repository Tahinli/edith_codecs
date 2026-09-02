# lane-interp3 r2 HANDOFF (turn cap)

Tip: see `git log -1` on branch `lane-interp3` (r2 fix commit + report commit).
Base: r1 b702853 + `git merge main` (4eff3a1, lane-mvtwin mvstack fixes). Merge touched
only mvstack.rs / stream.rs / lanes/mvtwin-r1.report.md; cdf.rs untouched both sides.

## Phase table + divergences: see lanes/interp3-r2.report.md (complete, 17 rows)
Two divergences found in `decode_inter_block8`'s compound arm, both FIXED:
1. decode.rs:19151 `read_compound_ref_frames` got a hard-coded `LAST_FRAME` (or -1)
   for the above/left neighbour reference instead of the real per-mi bands
   (`above_ref0`/`left_ref0`, what `decode_inter_block` passes) -> every
   `av1_collect_neighbors_ref_counts` vote off the wrong CDF row.
2. decode.rs:19410 the leaf's grid stamp cleared BOTH per-slot `is_global_mv_block`
   flags; now computed as libaom blockd.h:421-429 (mode GLOBAL_GLOBALMV + that slot's
   gm model > TRANSLATION).
Also added: decode.rs:19726 per-entry `EC_STACK` print in the leaf's SINGLE-ref arm.

## Ladder (EC_TRACE_MODE=1, ours vs instrumented aomdec, RANGE never tell)
- Before fix 1, frames=5 stream: first divergence at compound leaf mi (10,2) --
  both enter the mode info at rng=61097; aomdec reads refs (LAST,GOLDEN) mode
  NEAREST_NEARESTMV -> rng 49516; ours read (LAST,LAST2) NEW_NEWMV -> rng 50496.
- Before fix 2, frames=16 stream: first divergence at single-ref 8x8 leaf mi (8,8),
  entry rng=43099 both; aomdec stack = [(-1,-1) w648, (0,0) w644] (2 entries),
  ours = [(0,0) w644, (2,-4) w644, (-1,-1) w4] (3) -- the extra (2,-4) is the
  neighbour's stored mv1 where libaom substitutes the reader's global mv.
- AFTER both fixes: 570 vs 570 mode elements, `diff` EMPTY, no refusal.

## Gate state
`a_real_aomenc_dual_filter_obmc_8x8_inter_sequence_decodes_pixel_exact` still
`#[ignore]`d (ignore text updated with this measurement). Now RED on RECONSTRUCTION,
not entropy: luma-only, frames 0-6 byte-exact, frame 7 = 36 Y wrong (max |d| 5, first
(42,15), the 8x8 leaf at mi (10,4), 1-px spill across its left/bottom deblock edges),
propagating to 222 Y + 4 U + 1 V at frame 15.
Artifacts: scratchpad .../{pin.obu (5 frames), pin16.obu==mm.obu (16), aom16.trace,
ours16e.trace, mm.aom.yuv, mm.ours.yuv, aom.obmc}.

## Suite
Unit `interp3-suite-r2-1788327820.service`, log `$HOME/.cache/interp3-suite-r2.log`.
Armed at the r2 fix commit; still running at the cap -- read the log's `test result:`
line, do not re-run blindly.

## EXACT NEXT STEP
Build an `EC_OBMC` emitter in decode.rs (both OBMC call sites) printing the oracle's
own byte format -- `EC_OBMC above|left mi=(r,c) wh=(WxH) rel=%d op=%d nbmv=(y,x)
nbref=%d nbbsize=%d filt=%d` -- then diff it against the 325 lines of
`EC_OBMC=1 aomdec --rawvideo -o /dev/null mm.obu`, starting at `mi=(10,4)`. The leaf
now records a REAL filter pair where it used to record the [3,3] sentinel, so an OBMC
blend over a compound neighbour is the first suspect for the residue.
