# lane-mergefix r4 report

## Verdict
The r3/charter premise is DISPROVED and a real root cause shipped: decode-order frame 1
of the pinned stream reconstructs BIT-EXACT (0 px) -- it is not a compound-prediction
defect. Our LOOP FILTER was filtering an interior transform-edge lattice the reference
never touches, because `edge_params` omitted spec 7.14.2's skip/`pu_edge` suppression.
Fixed; frame 1 post-deblock goes 7244 -> 177 wrong luma px (the 177 residue is the
bottom straddling band, rows 61-67, and is NOT closed). The straddling gate still
fails, but on a DIFFERENT, now-isolated defect (see residue) -- its reported count is
unchanged at 3622 because the gate compares SHOWN frames, and its "frame 1" is
decode-order frame 2.

## Root cause + fix (crates/ec-av1/src/decode.rs)
- `edge_params` (~13300): a transform edge between two SKIPPED INTER blocks is filtered
  only when it is also a prediction (coded-block) edge -- libaom `set_lpf_parameters`
  `(curr_level || pv_lvl) && (!pv_skip || !curr_skipped || pu_edge)`. The old comment
  declared the term inert under "a coded block's transform is always its own full size";
  var-tx broke that invariant, so every var-tx 8-px interior edge inside skipped inter
  blocks was being filtered.
- `Neighbours::blk_org_grid` (new, ~4202/4309) + `fill_skip_grid_rect` (~4719): per-4x4-mi
  origin of the covering coded block -- the `pu_edge` term. Written where the skip flag
  already is (same `at_mi`, same call sites), so no new call-site sweep.
- `EC_LFPARAMS` rung (~12157): frame-level loop filter params, the instrument that ruled
  the filter LEVEL out.

EVIDENCE: ~/.cache/mergefix-tmp/r4_{pre,aom,apd,ours,fix}.f* | ours
`EC_AV1_PREFILT_DUMP16`/`EC_AV1_POSTDEBLOCK_DUMP16` vs aomdec `EC_AV1_PREFILT_DUMP`/
`EC_AV1_POSTDEBLOCK_DUMP` on str61.obu (md5 a14892ed0ba88b6ad2b566e251ea2d33; aomdec's
postdeblock dump is the mi-ALIGNED 192x72 buffer -- crop to 68 rows first; its
`*_DUMP16` rung SEGFAULTS on an 8-bit stream) | decode-order frame 1: pre-filter 0 px
both before and after; post-deblock 7244 (unfixed) -> 255 (suppression
without the `pu_edge` term) -> 177 (SHIPPED, with it). Frame 0 is 0 in every build. The
177 residue is entirely rows 61-67, the bottom straddling band, cols 130-191: we filter
143 px libaom leaves, libaom filters 15 we leave -- an open sub-defect, not closed.

## Gates
`cargo test -p ec-av1 --lib -j3 -- a_frame_edge_straddling_band_decodes_pixel_exact real_aomenc_1to4_streams_decode_pixel_exact_and_rect_vartx`
-> `1 passed; 1 failed` (log `$HOME/.cache/mergefix-r4-gates.log`): the 1:4 rect-vartx
sibling gate PASSES (no r3-style trade), the straddling gate still fails with
`frame 1 plane Y: 3622 pixels differ, first at row 0 col 64 (ours 56 vs ffmpeg 178)`.
Sibling batch (the 9 gates named in the r2 report, one invocation):
`$HOME/.cache/mergefix-r4-sib.log`.

## Residue (now precisely isolated, fix-now for r5)
Decode-order frame 2 (the gate's SHOWN frame 1) is wrong BEFORE any filtering: 1306
luma px, all inside the single 32-px column band x=96..127, rows 0..63; first at
row 0 col 97 (ours 42 vs aomdec 8; col 96 matches). That is the remaining prediction
defect the straddling gate reports. Second residue (open): 177 px in rows 61-67 (bottom
straddling band, cols 130-191) of decode frame 1's post-deblock output ON THE SHIPPED
TREE -- a horizontal edge at y=64 we filter and libaom does not (or vice versa, 15 px);
suspect the pu_edge/tx grid for blocks in the partial last mi row.

## Disposition
- fix-now (r5): the decode-frame-2 32-px prediction band above.
- accepted: r3's partition-context finding stays unshipped (sibling-gate trade).

## Sibling gate batch (one invocation, after the fix)
`cargo test -p ec-av1 --lib -j3 -- <the 9 r2 sibling names> a_frame_edge_straddling_band_decodes_pixel_exact real_aomenc_1to4_streams_decode_pixel_exact_and_rect_vartx`
-> `test result: FAILED. 10 passed; 1 failed; 0 ignored` in 108.06s
(log `$HOME/.cache/mergefix-r4-sib.log`). The single failure is the lane's own straddling
gate at its pre-existing 3622 px; no sibling traded.
EVIDENCE: $HOME/.cache/mergefix-r4-sib.log | the 11 gates above in one invocation on the
shipped tree | 10 passed, 1 failed (straddling, unchanged 3622 px).
