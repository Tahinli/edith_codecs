# lane-interp3 r3 HANDOFF (turn cap) -- root cause FOUND and FIXED, suite still running

Tip: `f36be26` on branch `lane-interp3` (fix) + this handoff/report commit.

## Suite state
- r2 suite (unit `interp3-suite-r2-1788327820.service`, log `$HOME/.cache/interp3-suite-r2.log`),
  armed at the r2 commit, FINISHED:
  `test result: ok. 378 passed; 0 failed; 34 ignored; 0 measured; 0 filtered out; finished in 742.50s`
- r3 suite FINISHED GREEN at f36be26 (unit `interp3-suite-r3-1788328778.service`):
  `test result: ok. 379 passed; 0 failed; 33 ignored; 0 measured; 0 filtered out; finished in 1031.33s`
  (old text below is superseded)
- r3 suite armed at f36be26: unit `interp3-suite-r3-1788328778.service`,
  log `$HOME/.cache/interp3-suite-r3.log`. At the cap it had 258 `... ok`, ZERO
  `FAILED`/`panicked`, no `test result:` line yet. READ THAT LINE, do not re-run
  blindly, then paste it into `lanes/interp3-r3.report.md` where
  `EVIDENCE: SUITE_LINE_PLACEHOLDER` sits.

## Stage attribution of the frame-7 residue (done FIRST, as chartered)
`EC_AV1_PREFILT_DUMP` on both decoders over the pinned 16-frame stream
(`.../scratchpad/mm.obu`), byte compare per decode-order frame:
frames 0-6 identical, frame 7 = 35 differing luma bytes, 8..15 grow to 224.
=> PRE-FILTER, i.e. prediction/reconstruction, never the loop filter/CDEF/LR.
All 35 sit in luma rows 42-47, cols 16-23, i.e. the block at mi (10,4);
rows 40-41 of that same block are EXACT and the error grows downward.

## What the OBMC comparison actually said (emitter built, class swept, CLEAN)
Before writing the emitter, the hand comparison against libaom found NO mismatch:
`overlappable_above`/`overlappable_left` stepping and `max_neighbor_obmc`
(`{0,1,2,3,4,4}`, 8x8 -> 1 per pass) match `obmc.h:20-55`; overlap sizes
(4 luma rows above / 4 luma cols left at 8x8, chroma 2) match
`build_obmc_inter_pred_{above,left}`; `av1_skip_u4x4_pred_in_obmc` chroma-above
skip matches; `obmc_mask_1/2/4/8/16/32` are byte-identical to
`reconinter.c:752-765`; OBMC never warps (`if (!build_for_obmc) av1_init_warp_params`).
One real divergence candidate was left un-ported and is still a documented
corner-cut, not a defect on this content: `obmc.h`'s `mi_step == 1` 4-wide pair
merge, and libaom's `op_mi_size = AOMMIN(xd->width, mi_step)` which can exceed
the remaining span while ours clips with `.min(end_col - col)` (only reachable
for a 2nd neighbour with a wide step).

Emitter: `decode.rs:15806` `ec_obmc_bsize()` + `decode.rs:15828` `ec_obmc_trace()`,
called at both passes in `obmc_blend`. Format is aomdec's own bytes:
`EC_OBMC above|left mi=(r,c) wh=(WxH) rel=N op=N nbmv=(y,x) nbref=N nbbsize=N filt=N`
(`filt` = packed `interp_filters.as_int` = `y | (x << 16)`).
`diff` of the 325 oracle lines vs our 325: EMPTY. OBMC is exonerated.

Also added `decode.rs:14682` + `:22658` `EC_PICT idx=` (decode-order frame
marker under `EC_OBMC`/`EC_TRACE_MODE`). REQUIRED: the marker prints AFTER the
frame's own trace lines, so `awk` must count markers seen, not read the value on
the preceding line -- attributing by the preceding `EC_PICT` value is off by one
and cost part of this round.

## Hypotheses ruled out (each by measurement)
- OBMC neighbour fields / masks / overlap: 325/325 identical lines (above).
- Frame 7 has NO OBMC block at mi (10,4) at all -- it is COMPOUND
  (SIMPLE_TRANSLATION), so the r2 handoff's "OBMC over a compound neighbour"
  suspect could not have been it.
- Residual/entropy: r2's range ladder is exact end to end; `EC_MCDUMP` (a
  temporary rung, removed before commit) showed aom_recon - our_residual
  reproduces our PREDICTION in rows 40-41 and diverges below => prediction.
- Wrong dual-filter x/y assignment: a swap would err in every row, not only the
  lower ones.

## Root cause (FIXED, gate GREEN)
`decode_inter_block8`'s COMPOUND arm predicted BOTH taps translationally.
libaom applies `allow_warp`'s GLOBAL branch to every `is_global_mv_block`
reference slot -- compound included, independent of `motion_mode`
(`av1_init_warp_params` inside `build_inter_predictors_8x8_and_bigger`'s per-`ref`
loop; `reconinter.c:33-55`). `decode_inter_block`'s compound arm has done this
since lane-cwarp r1 (`decode.rs:16691`). Third twin-functions-drift instance in
this lane. Frame 7 mi (10,4): GLOBAL_GLOBALMV, ref0=GOLDEN mv (0,0),
ref1=ALTREF mv (-3,4), comp_group_idx=1.
Fix: `decode.rs:19513` (`leaf_compound_warp`, `warp0_c`/`warp1_c`),
`decode.rs:19596`/`:19618` (`warp_affine_compound` per luma tap; chroma is 4x4,
below `av1_init_warp_params`'s 8px per-plane bound, so it stays translational).
Counter `COMPOUND_WARP_HITS_8` / `compound_warp_hits_8()` at `decode.rs:1786`.
After the fix ALL 16 decode-order frames are byte-identical pre-filter.

## Gate
`a_real_aomenc_dual_filter_obmc_8x8_inter_sequence_decodes_pixel_exact`
(`stream.rs:6332`) is UN-IGNORED and GREEN with four hard asserts (compound-8x8
filter read, differing dual-filter directions, 8x8 OBMC blend, 8x8 compound
GLOBAL warp). `$HOME/.cache/interp3-gate-r3.log`: `1 passed; 0 failed`.

## EXACT NEXT STEP
1. DONE (suite green, report filled). Nothing owed here.
2. Then the lane's open residue is only the two deferrals in the report
   (10-bit arm blocked by lane-inter8's 10-bit 8x8-leaf desync; `--tile-columns=1`
   arm blocked by the SB-level rect-partition refusal).
