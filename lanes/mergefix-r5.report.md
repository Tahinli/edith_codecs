# lane-mergefix r5 report

## Verdict
`a_frame_edge_straddling_band_decodes_pixel_exact` is GREEN. Root cause: the frame's
SAVED motion field (`build_motion_field`, this crate's `av1_copy_frame_mvs`) read the mi
grid through `MiGrid::get`, which is clamped to the CURRENT tile window -- and it runs
after the tile loop, so only the LAST tile's columns were ever stored. Every earlier
tile's temporal (MFMV) candidates were then absent from the next frame's MV stack.

## Fix (2 files)
- `crates/ec-av1/src/mvstack.rs` (`MiGrid::get_frame`, new, after `get`): unclamped
  frame-level read.
- `crates/ec-av1/src/decode.rs` (`build_motion_field`): reads `get_frame` instead of `get`.
- Rungs kept (they are what found it): `EC_TPL` (stream.rs, per inter frame:
  `order_hint`, `use_ref_frame_mvs`, filled projected cells) and `EC_TPL_SAVED`
  (decode.rs under `EC_TRACE_TPL`, occupancy map of the saved field, via
  `MotionField::occupancy` / `TplField::filled_cells`).

## Why the entropy stream never showed it
The mode/mv ladder of decode frame 2 is element-identical to the oracle (30 blocks, same
`rng` at every `EC_MODE`). A missing weight-2 temporal vote only REORDERS the stack: at
mi(0,24) (NEW_NEWMV, ref 1+7) aomdec's stack[1] is `this=(0,0) comp=(0,0) w=34` (17
temporal probes x 2), ours was `this=(36,0) w=2` -- the compound-extension zero-fill --
so the NEWMV predictor was 36 off and the decoded mv came out (32,0) vs (-4,0).

EVIDENCE: ~/.cache/mergefix-tmp/r5/{o16,n16,aom}.f0-5 | ours `EC_AV1_PREFILT_DUMP16`
(cropped 192x68 u16 LE) vs aomdec `EC_AV1_PREFILT_DUMP` (192x68 u8) on str61.obu (md5
a14892ed0ba88b6ad2b566e251ea2d33) | pre-filter wrong luma px per decode-order frame
0..5: 0/0/1306/3358/5027/6622 BEFORE -> 0/0/0/0/0/0 AFTER.

EVIDENCE: ~/.cache/mergefix-tmp/r5/{o,a}.tpl | `EC_TRACE_TPL=1` on both decoders |
aomdec has `EC_TPL mi_row=0 mi_col=24 blk=(0,0) mfmv0=(0,0) rfo=4`, ours INVALID; our
`EC_TPL_SAVED` occupancy map showed every frame's saved field filled only in x8 columns
16..23 (mi_col 32..47 = the last of the 2 tile columns) out of 24.

## Gates (one invocation, log $HOME/.cache/cargo-target-mergefix-r5-gates.log)
`cargo test -p ec-av1 --lib -j3 -- a_frame_edge_straddling_band_decodes_pixel_exact <9 r2
siblings> real_aomenc_1to4_streams_decode_pixel_exact_and_rect_vartx`
-> `10 passed; 1 failed`. The lane gate and all 9 siblings PASS (incl. both 10-bit arms,
sub8x8 inter split, 16x16 AB, split-transform SB/HORZ-VERT strips, 4:1/sub8 chroma strip).

The one failure, `real_aomenc_1to4_streams_..._rect_vartx_leaves_fire_before_a_named_refusal`,
is PRE-EXISTING on the merged main state, not a trade: with the one-line fix reverted the
gate fails identically ("never fired (32x16=0, 16x32=0, 0 refusals, 0 compared)", 18
attempts, 0 mismatches) -- log `$HOME/.cache/cargo-target-mergefix-r5-prefix.log`. It is
the gate-recipe defect the ledger records after lane-rectres lifted its documented blocker.

## Class sweep
Class: a frame-level consumer reading a per-tile-clamped map. Swept every `grid.get(`
in the crate: the other 20 sites are per-block neighbour scans inside the tile walk
(correctly clamped); `build_motion_field` is the only frame-level reader after the tile
loop (decode.rs:27980 is the only `grid` use past it).

## Disposition
- accepted: `real_aomenc_1to4_streams_..._rect_vartx_leaves_fire` stays red (pre-existing
  recipe defect, needs an aomenc recipe that emits a 64x16/16x64 var-tx leaf).
- accepted: r3's partition-context finding stays unshipped (sibling-gate trade, r4).
- closed: r4's 177-px post-deblock residue in decode frame 1's bottom band -- gone with
  this fix (all frames 0 px pre-filter; the gate compares post-filter and is green).
