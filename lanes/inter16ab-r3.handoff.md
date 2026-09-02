# lane-inter16ab r3 — HANDOFF (turn cap)

## Merge state
- `main` (8e9ba81) merged: commit `Merge branch 'main' (8e9ba81) into lane-inter16ab` (clean).
- `lane-r14` **b86eb38 was NOT in main**; merged directly. Two conflicts, resolved keeping
  BOTH sides: `examples/decode_probe.rs` (both counter blocks) and `decode.rs`'s
  `read_var_tx_size` split arm — r14's side taken there, which is the general
  `(sub_w, sub_h) = sub_tx_size_map(tx_w, tx_h)` recursion and DELETES both split-transform
  refusals (the 64-axis one and this lane's 16x4 one). `cdf_state.rs` auto-merged, no
  hand-resolution.

## Root cause of the r2 attempt-5 desync — FOUND AND FIXED (commit a506474)
Repro without the gate (stream `~/.cache/inter16ab-tmp/a5.obu`, md5 525b8e4ebfb9b347d990d35eeb890586,
recipe = gate attempt 5: 192x128 6 frames 8-bit cq34 X-source `--enable-tx-size-search=1`):
- aomdec `EC_TRACE_MODE` vs our `EC_TRACE_MODE`: identical through block mi(24,0) (a 16x8
  compound strip, ref0=4 ref1=7, `comp_group_idx=1` per `EC_AV1_COMPIDX_DUMP`), then the NEXT
  block's entry range is ours 64901 vs oracle 47271 — divergence inside mi(24,0) after the mv.
- `decode_inter_block` carries the enclosing SQUARE `side`, and the masked-compound read took
  its CDF row from it (`match side { 8 => 3, 16 => 6, 32 => 9 }`), i.e.
  `compound_type_cdf[BLOCK_16X16]` where libaom `decodemv.c::read_compound_type` uses
  `compound_type_cdf[bsize]` = BLOCK_16X8. Same class at the single-ref wedge-interintra row
  (`if side == 16 { 6 } else { 9 }`) — fixed too.
- Fix: `bsize_all_index(bw, bh)` + `wedge_used_bsize(bw, bh)` in `decode.rs` (just above
  `is_any_masked_compound_used_here`), both rows now from `write_w/write_h`. The wedge MASK
  codebook (`wedge.rs`) is square-only, so a rect COMPOUND_WEDGE / wedge-interintra block is
  now a NAMED REFUSAL (2 new `refusal_inventory` entries; that test is green).
- EVIDENCE: `~/.cache/inter16ab-tmp/{a5.obu,ours.yuv,ref.yuv}` | rebuild + `EC_PROBE_OUT=ours.yuv decode_probe a5.obu`, `ffmpeg -i a5.obu -f rawvideo ref.yuv` | 6/6 frames byte-IDENTICAL (was 8683 luma px wrong in frame 5), and `inter16_1to4: horz4=8 -> 0`.
- CLASS CONFIRMED (phantom counters): the oracle's `EC_TRACE=1` `EC_PART_VAL` histogram on that
  stream is `bsize=6 {0:570, 1:5, 2:1}` + splits only — **zero 1:4 partitions**. Our 8 "horz4"
  hits were phantom reads of an already-diverged stream, so the gate's `fired==0` out-of-scope
  guard could not catch it.

## Gate state after the fix — still RED, but for COVERAGE, not correctness
`$HOME/.cache/inter16ab-gate-r3.log` (unit inter16ab-gate-1788328988):
- 8-bit: 3 named refusals, **1** pixel-exact attempt carrying a real 1:4 partition, arms
  `[horz4=1, vert4=0, chroma_pair=1, sub8x8=1]`, 28 attempts carried none, **oos_mismatch = 0**.
- 10-bit: 1 named refusal, **0** attempts carried a 1:4 partition, **oos_mismatch = 0**.
- Panic is the final `arms_total.iter().all(> 0)` assert at `stream.rs:8227` — VERT_4 never
  fires and the 10-bit sweep never picks a 1:4 at all. NO pixel mismatch anywhere any more.
- One 8-bit attempt (attempt 1) now stops on the new
  `a wedge interintra mask on a rectangular inter block` refusal — real, previously mis-decoded.

## EXACT NEXT STEPS
1. Widen the recipe so VERT_4 and a 10-bit 1:4 actually fire (Y-structured source is in the
   sweep via `natural`, but the cq/tx/sp axes clearly do not reach VERT_4): sweep cq/sb-size/
   `--max-partition-size=16` variants OUTSIDE the test first (script pattern in this handoff's
   repro command), pin the ones that fire, then narrow the gate to them. Do NOT weaken the
   arms assert.
2. Rect wedge support (lifts the two new refusals): `wedge.rs` builds masks for side 8/16/32
   only; libaom `av1_init_wedge_masks` builds them for BLOCK_8X16/16X8/16X32/32X16/8X32/32X8
   from the same master mask.
3. Split transform on a 16x4 strip (charter item 3) — NOT DONE. The refusal is already gone
   (r14 merge), so a TX_16X4 -> TX_8X4 split now reaches `rect_inter_residual_supported(8,4)`
   which is FALSE, and `rect_inter_luma_set` would hit `unreachable!()`. **This is a panic
   risk on the merged tree and must be closed before merging to main.** Work needed:
   `rect_inter_residual_supported` += `(8,4)|(4,8)`; `rect_inter_luma_set` += a new
   `TxbSet::LumaRect8x4Inter{,1}` — shape = `LumaRect4x8`'s (Luma8 coefficient tables,
   `eob_pt_32_luma` + `eob_pt_32_luma_class1`, 32 positions) with `inter_tx_type_4` /
   `inter_tx_type_4_set1` at the TX_4X4 row (`av1_get_ext_tx_set_type(TX_8X4, inter)` =
   DCT_IDTX reduced / ALL16 unreduced); `SCAN_8X4`/`SCAN_4X8` already exist in `rect_scan`.
   Then a counter + a `--enable-tx-size-search=1` gate arm.
4. Full suite armed as unit `inter16ab-suite-r3` -> `$HOME/.cache/inter16ab-suite-r3.log`
   (result not seen before the cap). Film `-ss 900` re-probe NOT re-run this round.
