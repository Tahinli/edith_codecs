# lane-rectx r3 — the rect leaf was bit-exact; the block AFTER it was not

Branch `lane-rectx`, on top of `main` 3808cf8 (`git merge-base --is-ancestor main HEAD` = OK,
no rebase needed). Prior state: r1/r2 landed non-skip TX_16X8/TX_8X16 rect-leaf coefficient
decode (`decode_leaf_rect`'s lane-rectx arm, `read_coeffs_rect`, `TxbSet::LumaRect16x8{,Set1}`
/`ChromaRect8x4`, `EOB_PT_128_LUMA`/`EOB_PT_32_CHROMA`, `SCAN_8X4`/`SCAN_4X8`) UNPROVEN, with
the lane's own gate red.

## What this round proved

1. **The r2 coefficient path is right.** Against the instrumented oracle
   (`~/.cache/aom-oracle/build/aomdec`, `EC_TRACE_COEFF=1`) on a real aomenc stream, a
   non-skip `TX_16X8` luma leaf plus both its `TX_8X4` chroma halves decode with an
   IDENTICAL msac range at every element (`all_zero` 37445, `tx_type` 55512, `eob`=1 41024,
   `base_eob` pos 0 level 1 64544, DC sign 37072, chroma `all_zero`=1 40760 / 47390).
   Scans, eob CDF alphabet, txb sets, nz-map/br contexts and the DC-sign context at this size
   are all confirmed by that ladder, not by argument.
2. **`SCAN_8X4`/`SCAN_4X8` re-checked against the real `scan.c`** (not a memory of it):
   `~/.cache/aom-oracle/src/av1/common/scan.c` `default_scan_4x8` == our `SCAN_8X4` and
   `default_scan_8x4` == our `SCAN_4X8`, verbatim, which is what
   `decode.rs::tests::the_rect_scan_tables_match_libaom...` already pins (asymmetric pair, a
   swap fails it).
3. **Root cause of the desync (FIXED): the `use_filter_intra` flag was never read at
   16x8/8x16.** `av1_filter_intra_allowed_bsize` is "both sides <= 32", so libaom reads the
   flag for every DC_PRED 16x8/8x16 block; `filter_intra_size_class_rect` returned `None`
   there, so we dropped one symbol per DC_PRED strip and desynced the tile from that block on
   — silently, with a full-frame pixel mismatch and no error. Ladder evidence: leaf mi=(2,4),
   we and the oracle both decode `mode=0 uv_mode=9 skip=0`, but our post-mode range is 34808
   where the oracle's is 40668 (value-equal narrowing drift). After the fix our range at that
   point is 40668 and the next transform block's `all_zero` reads 40269 — bit-identical to the
   oracle.

## Files changed

- `crates/ec-av1/src/cdf.rs:415` — `FILTER_INTRA` 6 -> 8 rows, adding `default_filter_intra_cdfs`
  index 5 (BLOCK_16X8, 9394) and 4 (BLOCK_8X16, 12551).
- `crates/ec-av1/src/cdf_state.rs:467` — the field's type/doc follow.
- `crates/ec-av1/src/decode.rs` `filter_intra_size_class_rect` — `(16,8) => Some(6)`,
  `(8,16) => Some(7)`; the actual fix.
- `crates/ec-av1/src/decode.rs` `read_coeffs_rect` — three env-gated trace prints
  (`tag=all_zero_rect` with the pre-symbol range, `tag=sign_rect`) under the existing
  `EC_TRACE_COEFF`, plus the entry range on the `EC_AV1_RECTX_TRACE` line; these are what made
  the cross-decoder bisection possible and are kept for the successor.
- `crates/ec-av1/src/decode.rs` tests — new
  `a_rect_strip_below_16x16_reads_its_own_filter_intra_cdf_row`.
- `crates/ec-av1/src/stream.rs` `sweep_rectx_recipes` (scratch, `#[ignore]`) — also sweeps
  `--reduced-tx-type-set=1`, and only dumps the 8x8 mismatch map when there IS a mismatch.

## Checks

- `cargo test -p ec-av1 --lib -- filter_intra scan_` -> 8 passed, 0 failed (includes the new pin
  and the existing `a_real_aomenc_filter_intra_stream_decodes_pixel_exact`).
- `EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib` -> see TOTALS below.
- EVIDENCE: /tmp/claude-1000/.../scratchpad/{s.obu,aom.log,ours.log} | ffmpeg mandelbrot 64x64
  start_x=-0.6 -> aomenc cq16 --enable-rect-partitions=1 --min-partition-size=8
  --max-partition-size=32; instrumented aomdec `EC_TRACE_COEFF=1` vs
  `examples/decode_probe` `EC_TRACE_COEFF=1` | first divergence moved from the transform block
  after leaf mi=(2,4) (ours 34467 vs oracle 40269) to none at that point (both 40269) after the
  fix.

## Open residue

- **fix-now (next round, out of this round's tool budget): the rect leaf's `kf_y_mode`
  context is read from the coarse 16x16 neighbour grid.** Second, independent defect, found
  by the same ladder on `rgbtestsrc=size=64x64` cq=50 `--reduced-tx-type-set=1`: at leaf
  mi=(4,12) (`decode_leaf_rect`, bw=16 bh=8, outer_at=(1,3)) we and the oracle ENTER the block
  at the same range 59792, and then we decode `mode=0` where the oracle decodes `mode=2`
  (H_PRED) — a wrong CDF row, not a wrong value. `Neighbours::above_mode`/`left_mode`
  (decode.rs:1806) are per-16x16-cell; libaom takes `above_mi`/`left_mi` from the real mi grid,
  so when the cell above was split into 8x8 leaves our slot holds the last-written leaf's mode
  (bottom-RIGHT) instead of the true above neighbour (bottom-LEFT at mi col 12). Known class:
  "context read from one cell". The fix is a per-mi mode grid alongside the existing per-mi
  `skip_grid`/`lf_grid` — 13 `fill_skip_grid*` call sites would gain a mode write. It affects
  every intra path, not just rect leaves.
- **deferred(the mode-grid fix above): the lane gate
  `a_real_aomenc_stream_with_a_coded_rect_strip_below_16x16_decodes_pixel_exact` is still RED**,
  and its two refusals are NOT lifted. Its current recipe fails on a third, unrelated blocker
  ("a HORZ_A/HORZ_B/VERT_A partition below 16x16"): a 126-cell sweep (9 lavfi sources x 7
  cq x reduced-tx-type-set 0/1, `sweep_rectx_recipes`) found aomenc writes AB partitions below
  16x16 despite `--enable-ab-partitions=0` in most cells — same aomenc quirk the ledger already
  records one level up. After the filter_intra fix the sweep has exactly ONE cell that decodes
  to completion (rgbtestsrc cq=50 rtx=1, 4 rect leaves fired) and it mismatches on the mode-ctx
  defect above. So there is currently no recipe that can turn the gate green; a green gate
  needs the mode-grid fix first.
- accepted: `read_coeffs_rect` still refuses `V_DCT`/`H_DCT` ("a non-2D tx class on a
  rectangular transform"), which is why the gate recipe pins `--reduced-tx-type-set=1`.

TOTALS: see lanes/rectx-r3.handoff.md.
