# lane-golomb r7 HANDOFF -- residue CLOSED, gate GREEN, suite still running at the cap

Tip: `a548e9d` on `lane-golomb` (parent `0fcdf39`). Full report: `lanes/golomb-r7.report.md`.

## Suite r6 result line
There is none. `$HOME/.cache/golomb-suite-r6.log` was still `active` at 254/404 tests
(stuck in the long superres/encoder tests) and holding `CARGO_TARGET_DIR`, so r7 stopped
the unit to build -- it was measuring the PRE-fix tree and is superseded. Every `test`
line it did emit was `ok` or a documented `ignored`.

## Suite r7 -- INCOMPLETE at the turn cap
Unit `golomb-suite-r7` -> `$HOME/.cache/golomb-suite-r7.log`, still `active` at 180/404
tests, NO `test result:` line yet, no `FAILED` line so far. A successor must read that
log (`grep -E "^test result|FAILED"`); if the unit died, re-arm it with the COMMON recipe.
This is the one owed artifact of the round.

## What the picture-4 investigation showed (the ladder was not needed)
Method: `EC_TRACE_MODE=1 EC_TRACE_COEFF=1` on `g35.obu` (md5 `9037f5b21db95d35e71f91f040fc33e1`,
`scratchpad/gl-gen.sh 35`) through the r6-built `decode_probe`, stderr split into frames on
`EC_MODE mi_row=0 mi_col=0` -> segment 4 = decode-order picture 4
(`scratchpad/r7-ours.err`, still on disk).

* The residue block is `mi(0,16)` and the next `EC_MODE` is `mi(0,32)`, so it is a **64x64**
  block, NOT the 32x32 r6 recorded. Var-tx split into 16x16 luma leaves.
* Its last two coefficient units are `eob=1022 tx=DctDct` and `eob=1020 tx=DctDct`
  (`ctx=0`): eob > 256 proves **32x32 chroma** units
  (`av1_get_max_uv_txsize(BLOCK_64X64) = TX_32X32`).
* Its FIRST luma var-tx leaf is `eob=254 tx=Idtx`.
* No dequant-value ladder against aomdec was required: the coefficient values are provably
  identical (the whole stream stays in entropy sync and luma is bit-exact), so the only
  free variable left was the chroma `tx_type` -- and `IDTX` vs `DCT_DCT` are both
  `TX_CLASS_2D`, i.e. identical scan, identical contexts, identical msac ranges, different
  inverse transform. That is exactly "entropy in sync, one SB of high-frequency chroma
  noise, luma exact".

## Fix applied (root cause, libaom-cited)
`av1_get_tx_type`, `av1/common/blockd.h:1278-1309`, chroma of an INTER block:
1. `DCT_DCT` only when `txsize_sqr_up_map[tx_size] > TX_32X32` (STRICTLY greater);
2. else the colocated luma unit's coded type, `xd->tx_type_map[(blk_row<<ss_y)*stride + (blk_col<<ss_x)]`;
3. then `if (!av1_ext_tx_used[av1_get_ext_tx_set_type(tx_size, 1, reduced)][tx_type]) tx_type = DCT_DCT;`
   (`av1_ext_tx_used` blockd.h:1036, `av1_get_ext_tx_set_type` blockd.h:1097).

We had step 1 as `chroma side >= 32 -> DCT_DCT` and step 3 missing entirely. At exactly
`TX_32X32` an inter block reads `EXT_TX_SET_DCT_IDTX`, which CONTAINS IDTX -> the block
above must use IDTX on chroma; we used DCT_DCT.

`crates/ec-av1/src/decode.rs`: new `fn reduce_inherited_chroma_tx_type(t, w, h)` (just above
`fn inter_txbset_for`) implementing all four inter sets; both callers switched
(`read_inter_plane` and `read_inter_plane_rect` `default_tx_type`). Sweep: `rg
inherited_luma_tx_type` -- those are the ONLY two sites deriving a chroma tx_type for an
inter block; the intra sites (`decode.rs:6056`, `9195`) are correct as-is (intra at
`sqr_up == TX_32X32` really is `EXT_TX_SET_DCTONLY`).
Unit test added: `decode::tests::an_inter_chroma_transform_narrows_its_inherited_tx_type_to_its_own_set`
(`cargo test -p ec-av1 --lib -- an_inter_chroma_transform_narrows` => 1 passed).

## edge32 gate: every arm GREEN
`$HOME/.cache/golomb-gate-r7.log`: `test result: ok`,
`38 pixel-exact attempts, 32-level edge bits [horz_or_vert=0 split=394], 64-level edge bits
[horz_or_vert=7 split=197] right-VERT=7, 2 named refusals`.
Arms: (192x80, 80x192) x 1 frame x {8-bit,10-bit} plus 192x80 5-frame, cq
{35,40,45,50,55,57,59,61} -- so all eight cq levels are green at BOTH depths. The only two
non-compared attempts are `192x80 cq45/cq50 frames=5 8-bit`, which refuse by name on
another lane's feature (inter SB-level AB partition).
BEFORE: `192x80 cq35 frames=5 10bit=false frame 3 plane U: 978 pixels differ, first at row 0
col 32 (ours 33 vs ffmpeg 8)`.

## Ruled out this round (each with its measurement)
* r6's "32x32 block at mi(0,16)" premise -- WRONG, the trace shows a 64x64 block (the next
  EC_MODE is mi(0,32)); anything chartered off "32x32 chroma is 16x16" was mis-premised.
* A wrong chroma tx SIZE: the chroma eob of 1022 (> 256) proves our unit really is 32x32,
  which is what `av1_get_max_uv_txsize` says.
* A dequant / per-plane delta_q cause: f0-f3 chroma is bit-exact under the same quantizer.
* Everything r6 already ruled out (deblock/CDEF/LR, DIFFWTD, warp/GM/OBMC/dual filter/
  masked-comp/interintra/dist-wtd, narrow-block chroma MC) stays ruled out.

## Straddling-TU arm (r3 verifier ask): NOT added, and the two measurements say why
Both measured this round with the arms temporarily in `edge32_gate`
(`$HOME/.cache/golomb-gate-r7b.log`):
* `(192, 68, 5, false)`: ALL EIGHT cq attempts refuse by name -- `a reference picture whose
  height does not match this frame's own true size` -- so the arm compares nothing
  (vacuous; class `counter-from-refused-stream`). That refusal is the next blocker for any
  non-SB-multiple height.
* `(68, 192, 5, false)`: RED on a real, separate defect --
  `68x192 cq35 frames=5 10bit=false frame 1 plane Y: 141 pixels differ, first at row 0 col
  64 (ours 167 vs ffmpeg 166) [edge32=[0,34,0,0,1,17,0,1]]`. LUMA, inside the 4-px
  straddling COLUMN. The straddling-ROW twin never reaches this because of the refusal above.
The reason is recorded as a comment in `edge32_gate` (`crates/ec-av1/src/stream.rs`, just
above `let cqs`) so it is not re-attempted blind.

## Exact next step for r8
1. Read `$HOME/.cache/golomb-suite-r7.log`'s `test result:` line (N/0 expected); that is the
   only thing this round did not finish.
2. `main` has advanced to `85887c7` (this branch merged `18bf7dc`); re-merge before any
   further work.
3. Then the 68-px straddle: fix the `68x192` luma defect at the 4-px straddling column
   (start from `read_var_tx_size`'s `blk_col >= max_w_mi` early return and the right-edge
   prediction/availability, first difference is at row 0 col 64 = the first straddling
   column of decode picture 1), and the `192x68` "reference picture whose height does not
   match this frame's own true size" refusal -- then the arms can be added for real.
