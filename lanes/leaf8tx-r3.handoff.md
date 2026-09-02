# lane-leaf8tx r3 handoff (turn cap)

## ROOT CAUSE FOUND AND FIXED (decode.rs `tx_size_context_txfm`)

`crates/ec-av1/src/decode.rs:11943/11946` read the per-MI bands
`above_inter`/`left_inter` as `[mi_c / (SUB / MI)]` / `[mi_r / (SUB / MI)]`.
Those two vectors are written by `record_inter` as `[c + cell]` (per MI) and are
read as `[cmi]`/`[rmi]` at every other site (17426, 20183, 20614, 21180), so the
division read a column/row FOUR mi away. When that stale cell said "inter", the
neighbour contributed its 8-px BLOCK size instead of its 4-px TRANSFORM size and
`get_tx_size_context` returned 2 where libaom returns 1. Fix = index by `mi_c` /
`mi_r`. Only 2 sites in the file used the division (grep `/ (SUB / MI)]`); both
fixed, no sibling instances remain.

libaom reference: `pred_common.h` `get_tx_size_context` (above/left from
`above_txfm_context[0]`, overridden by `block_size_wide/high` only when the
neighbour `is_inter_block`).

## The r2 premise was WRONG -- do not chase txb_skip again
- The txb_skip CDF row is NOT divergent. libaom stores INVERTED cdfs: aomdec's
  `[15,0,32]` IS our `[32753,32768,32]`. Verified read-by-read: a private
  instrumented aomdec (`EC_TRACE_TXBCDF`, private build dir
  `~/.cache/leaf8tx-aombuild`, source patch reverted afterwards) printed
  `txb_skip_cdf[txs_ctx][ctx]` at every all_zero read; all 1220 luma
  (txs=0,ctx=6) reads carry cdfs identical to ours, and the per-class read
  histograms match exactly (aom `plane0 txs0 ctx6`=1220 = our `side=4 ctx=6`;
  aom chroma ctx 7/8/9 = our chroma rows 0/1/2).
- The real desync is one element EARLIER, in the MODE ladder: `EC_ISTEP`
  `name=tx_depth mi_row=30 mi_col=36` -- same range in (43044), same value (1),
  same ctx (2), different range out (ours 47744, aomdec 50432), because two
  blocks earlier at mi(20,46) we read ctx=2 where aomdec reads ctx=1 (identical
  range there by luck, class [[wrong-alphabet-same-value]]) and adapted the wrong
  `tx_size_cat0` row. Exactly 1 ctx mismatch in 449 tx_depth reads of the stream.
- Instrument fixed on our side too: the square coefficient path never traced the
  `br` rung, so the cross-decoder ladder was blind to base-range symbols and
  mis-reported the divergence. `EC_COEFF_STEP tag=br ...` added at decode.rs:3679.

## EVIDENCE
EVIDENCE: ~/.cache/leaf8tx-tmp/our2.yuv vs ref.yuv | EC_LEAF8TX_SPLIT=1 decode_probe on the pinned 10-bit stream s68.obu (md5 587b4f1d6fbc15249c0ddd0479d55fd7) vs aomdec --rawvideo | `cmp` byte-identical (PIXEL EXACT; r2's 1140/281/285 mismatching samples are gone)
EVIDENCE: ~/.cache/leaf8tx-tmp/our_istep4.log | EC_TRACE_MODE_STEP ladder ours vs aomdec | 0 tx_depth ctx mismatches after the fix (was 1 at mi(20,46))

## STATE / still owed
- The tx_depth-split REFUSAL IS STILL IN PLACE (decode.rs:22610, bypassable with
  `EC_LEAF8TX_SPLIT=1`); refusal_inventory.rs:72 unchanged. Lifting it needs the
  gate arms + a suite run, which the turn cap cut. The pixel-exact pin above is
  the proof that the lift is now unblocked.
- `a_real_aomenc_stream_with_cdef_and_sub16_inter_leaves_decodes_pixel_exact`
  (r2's continue-and-sweep conversion) was NOT run to completion: the r3 suite
  (`$HOME/.cache/leaf8tx-suite-r3base.log`, PRE-fix code) never printed a
  `test result` line before the cap; it shows 2 failures that predate this round's
  fix: `decode::tests::an_obmc_neighbour_with_no_recorded_filter_refuses_instead_of_panicking`
  and `refusal_inventory::tests::the_decode_path_refuses_exactly_the_listed_cases`
  (both look like r2 merge-resolution fallout of the OBMC refusal string, not
  entropy). Re-run the whole suite on the fixed tree FIRST.
- The angle-delta gate and cdef/sub16 gate were not re-run this round.

## Oracle source incident (fixed, read this before touching the oracle)
I ran `git checkout av1/decoder/decodetxb.c` in `~/.cache/aom-oracle/src` to drop
my own one-line probe and it wiped 50 lines of OTHER lanes' uncommitted
instrumentation (class [[checkout-ate-uncommitted-work]] again). Restored:
`scripts/instrument-aom-oracle.sh` re-applied rungs 3+11, and the remaining
hand-added prints (base/br in both `read_coeffs_reverse*`, all_zero, tx_type
outside the AOM_PLANE_Y branch, extended base_eob) were reconstructed from the
shared aomdec binary's format strings. Proof of restoration: a fresh trace from a
rebuild of the restored source is `cmp`-identical to the pre-incident
`~/.cache/leaf8tx-tmp/aom_coeff.log`. The SHARED `~/.cache/aom-oracle/build`
was never rebuilt.
