# lane-sub8x4 r1 HANDOFF (tip = `bb79462`, branch lane-sub8x4)

Two commits on top of c1df2f2 (= lane-intra16x4 r3):

* `1845281` merge of `367d873` (lane-sub8intra r2) + the sub-8x8 sibling-size fix
* `bb79462` compound 8x8 leaf early-return fix

## 1. STEP 0 -- how the merge was resolved

`git merge 367d873` conflicted in ONE file, `crates/ec-av1/src/mvstack.rs`, at 8 hunks
(4 functions x 2 hunks: `scan_row`, `scan_col`, `scan_row_compound`, `scan_col_compound`).
Every hunk was the SAME libaom clause ported twice, once per lane, with different comment
wording and `if/else` vs `usize::from` spelling -- **semantically identical on both sides**:

1. the outer-scan step-back, `mvref_common.c:155-158` (`scan_row_mbmi`) and `:203-206`
   (`scan_col_mbmi`): `if (abs(row_offset) > 1) { col_offset = 1; if ((mi_col & 0x01) &&
   xd->width < width_8x8) --col_offset; }` and its transpose;
2. the weight / `processed_rows|cols` guard, `mvref_common.c:172` and `:222`:
   `if (xd->width >= width_8x8 && xd->width <= n4_w)` / `if (xd->height >= n8_h_8 &&
   xd->height <= n4_h)` -- the `>= mi_size_*[BLOCK_8X8]` half is what both lanes added.

Resolution: kept the HEAD (intra16x4 r3) spelling of all 8 hunks, which cites the libaom
line numbers; the sub8intra rationale is preserved in this file. Both sides' `bw4 >= 2 &&
bw4 <= n4` / `bh4 >= 2 && bh4 <= n4` guard lines were already identical and auto-merged.
All four functions were then re-read whole against `mvref_common.c:143-236`: `end_mi`,
`use_step_16`, `len` floors (`width_16x16` / `width_8x8`), the weight `inc`, the
`processed_*` update and the `i += len` step all match.

`decode.rs`, `stream.rs`, `examples/decode_probe.rs` auto-merged clean (both lanes' code
kept: the intra 16x4 strip path + OBMC pair merge from intra16x4, `decode_intra_sub8_leaf`
+ the sub-8x8 chroma guards from sub8intra). Both env bypasses (`EC_SUB8INTRA_DECODE`,
`EC_INTRA16X4_DECODE`) are still in place; no refusal lifted this round.

`cargo check -p ec-av1`: clean. Named gates run after the merge
(`cargo test -p ec-av1 --lib -- obmc mv_stack 1to4 sub8 refusal_inventory gate_coverage`):
**32 passed, 2 failed, 5 ignored**. The two failures are the two gates the ledger already
records as known-red RECIPE defects on main -- `a_real_aomenc_inter_sequence_with_16x16_
level_1to4_partitions_decodes_pixel_exact` ("VERT_4 / split-tx-8x4 arms read 0") and
`real_aomenc_1to4_streams_..._rect_vartx_leaves_fire` ("the rectangular var-tx leaf arm
never fired"). Both fail on a zero counter, neither on a pixel mismatch.

## 2. Two defects found and fixed (both post-merge, both root-caused by ladder)

### (a) `decode_inter_sub8_rect2`: leaf 1 never saw leaf 0's block size (`1845281`)

libaom's `xd->above_mbmi`/`left_mbmi` are the neighbour block AS DECODED, so leaf 1 of an
8x8 HORZ/VERT group sees leaf 0's own bsize. We published `above_side_mi`/`left_side_mi`
only in the GROUP tail (after both leaves), so leaf 1 read the block preceding the whole
group. `get_tx_size_context` (`pred_common.h:342`) takes `block_size_wide[above_mbmi->bsize]`
from there.

Witness `~/.cache/sub8intra-tmp/g_8_48_128x128.obu`, mi(25,2) (intra 8x4 leaf 1):
ours `tx_depth` **ctx=1** vs aomdec **ctx=2**, same decoded value 1, rng 35938 vs 55960.
`EC_TXCTX=1` (new env print in `tx_size_context_txfm_rect`) showed `above_side=4` where the
above neighbour is an inter 8x4 leaf of width 8. Class `tx-grid-published-block-side`.

Fix: publish the leaf-0 side bands at the top of iteration `i == 1` of the leaf loop
(`decode.rs`, `decode_inter_sub8_rect2`). The group tail rewrites the same values, so
partition context is unchanged.

### (b) `decode_leaf8` COMPOUND arm's early return clobbered its own per-TU luma ctx (`bb79462`)

The compound arm `return`s before the fall-through tail, and that tail is where the
`split8` plane-0 save/restore around `record_mi` lives. A compound var-tx 8x8 leaf therefore
published the whole-block luma grid (all-zero when the transform is split into 4x4 units,
since `read_inter_luma8`'s `record_mi_luma` already wrote the per-unit state) into
`above[..][0]`/`left[..][0]`.

Witness `~/.cache/intra16x4-tmp/g_8.obu` frame 2 mi(14,12), first luma TU: ours
`txb_skip` ctx=3 rng=43376 vs reference ctx=6 rng=61310 -- `get_txb_ctx`
(`txb_common.h:330`) `skip_contexts[4][0]=3` vs `[4][4]=6`; the block left of it, mi(14,10),
is a compound 8x8 whose four TX_4X4 units all read ctx=6 correctly and then had their
published levels zeroed. Class `early-return-skips-tail`.

Fix: the same save/restore in the compound arm before/after its `record_mi`.

## 3. Measured state (probe sweep, both bypasses on)

`~/.cache/sub8x4-tmp/check.sh <obu>...` (decode + `cmp` vs `ffmpeg -f rawvideo`),
12 sub-8x8-intra streams `~/.cache/sub8intra-tmp/g_{8,10}_{48,55,63}_{128x128,192x128}.obu`:

| state | before this round | after (a) | after (b) |
|---|---|---|---|
| pixel-EXACT (6/6 frames) | 5/12 | 10/12 | 10/12 |
| stops at a refusal | 2/12 | 2/12 | **1/12** |

Remaining non-EXACT: `g_8_63_128x128.obu` (mismatch) and `g_10_55_192x128.obu` (was
"Golomb tail longer than this decoder reads", now decodes all 6 frames and mismatches).

`~/.cache/intra16x4-tmp/g_8.obu` is now **entropy-exact end to end**: 2215/2215 `all_zero`
units identical in value AND range vs instrumented aomdec, with equal counts -- yet its
pixels still differ (frame 1 first at luma (64,88), 1185 px, max delta 20; frames 2..5
150-235 px each). That is a **reconstruction-only** residue (prediction / OBMC blend /
loop filter), not entropy. This supersedes the r3 handoff's section-2 diagnosis, whose
named cause (the 8x8 compound var-tx read) is now fixed.

## 4. STEP 1 list, unchanged / re-measured

* (a) sub-8x8 INTRA leaf reads `use_filter_intra` (+ `filter_intra_mode`): **premise was
  STALE -- already implemented** by sub8intra r2 in `decode_intra_sub8_leaf`
  (`decode.rs:23101`). Proven: with the new `EC_TRACE_MODE_STEP` prints added there, ours
  reads `use_filter_intra=1 rng=51020` then `filter_intra_mode=3 rng=45380` at mi(25,2),
  **bit-identical to aomdec**. `av1_filter_intra_allowed_bsize` is `reconintra.h:68`
  (`block_size_wide <= 32 && block_size_high <= 32`), so 8x4/4x8/4x4 are all allowed. No
  angle delta below 8x8 is likewise already implemented. What was actually wrong at that
  block was the tx_depth CDF ROW -- fix (a) above.
* (b) 8x8 COMPOUND inter leaf var-tx `txfm_split`: **premise partly stale** -- we DO read
  the symbol (ours `txfm_split val=1 ctx=19 rng=62656`, reference reaches the same 62656).
  The real defect was the `txb_skip` ctx, fix (b) above. DONE.
* (c) the flagged all-inter sub-8x8 chroma `first_tx_type` rule (`blockd.h:1288`,
  tx_type_map rebased to the chroma-reference = LAST sub-block): **NOT MEASURED this
  round.** Still open.

## 5. Next step (r2)

1. Root-cause the two remaining non-EXACT sub-8x8 streams and `g_8.obu`'s
   reconstruction-only residue (entropy is exact, so start at prediction: OBMC blend,
   sub-8x8 chroma MC, or the deblock grid a sub-8x8 group publishes).
2. Then the gates: **no sub-8x8-intra gate test exists yet in `stream.rs`** (only the
   `sub8_intra_rect_hits()` accessor at `stream.rs:91` and the 1:4 gate's refusal count).
   It must be WRITTEN, continue-and-sweep, over the now 10 EXACT arms of
   `~/.cache/sub8intra-tmp/sweep3.sh` (`--max-partition-size=8 --enable-1to4-partitions=0
   --min-partition-size=4`, 8/10-bit x cq 48/55/63 x 128x128/192x128). Only then lift
   `EC_SUB8INTRA_DECODE` + the two refusals at `refusal_inventory.rs:35-36`.
3. intra 16x4: still no EXACT witness; re-run `~/.cache/intra16x4-tmp/sweep_r3.sh` (it is
   idempotent and appends) now that both fixes landed -- r3 measured 0 EXACT of 49 rows
   BEFORE them.
4. Not run this round (turn cap): full `cargo test -p ec-av1 --lib` suite, film probes.

## 6. New instrumentation on this branch (env-gated, keep)

* `EC_TXCTX=1` -- `tx_size_context_txfm_rect` prints `above_txfm/left_txfm/above_inter/
  left_inter/above_side/left_side/above/left`; named defect (a) in one run.
* `EC_TRACE_MODE_STEP` now also prints `use_filter_intra` / `filter_intra_mode` from
  `decode_intra_sub8_leaf` (it read them silently before, which is what made the r2
  handoff conclude they were unread).
* `EC_AV1_TRACE` `luma_skip_ctx` line now dumps the whole `left[..][0]` level band;
  that dump is what localised defect (b).
