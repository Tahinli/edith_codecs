# lane-av1422llpred — 4:2:2 chroma intra prediction divergence

Target: `probe/ll422_allintra.obu` (320x240, 4:2:2 lossless all-intra, 1 frame).
Base: `2005a35d` (both prior fixes already landed). Baseline reproduced exactly:
**Y 0 / U 3126 / V 3139**.

## Root cause

`read_intra_chroma_lossless` computed a per-transform-unit `Reach` with
`tu_reach(side, side, col_off, row_off, 4 << ss_x(fctx), …)`. `tu_reach` passes its
single transform-size argument as **both** `tx_w` and `tx_h`
(`crates/ec-av1/src/encode.rs:1578`, `Reach::of_tu(bw, bh, col_off, row_off, tx, tx, block)`),
so the unit's transform height was shifted by `ss_x` instead of `ss_y`.

A TX_4x4 chroma unit is `4 << ss_x` luma px wide but `4 << ss_y` luma px tall.
At 4:2:0 (`ss_x == ss_y == 1`) and 4:4:4 (`ss_x == ss_y == 0`) the two shifts are
equal and the bug is invisible — which is exactly why both formats are byte-exact.
**Only 4:2:2 (`ss_x == 1`, `ss_y == 0`) is reachable for the defect.**

### The named expressions

| | expression | value at the first wrong unit |
|---|---|---|
| libaom | `av1_dr_prediction_z3_c`, `reconintra.c:609`; `dy = dr_intra_derivative[270-200]` = `dr_intra_derivative[70]` = **23** (`reconintra.h:122` / `:141`, table `reconintra.h:81`) | — |
| libaom reach | `has_bottom_left` early return, `reconintra.c:411`: `row_off + tx_size_high_unit[TX_4X4] < AOMMAX(mi_size_high[bsize] >> ss_y, 1)` → `0 + 1 < 2` → **1** | `n_bl=4` (oracle `EC_PRED` rung) |
| ours (was) | `Reach::of_tu` below-left chain, `encode.rs:4041`: `row_off + tx_h < bh` → `0 + (4<<1=8) < 8` → **false** → fell through to `block.below_left` = **false** | left edge sliced to 4 samples |
| ours (now) | `4 << ss_y(fctx)` → `0 + (4<<0=4) < 8` → **true** | `n_bl` equivalent |

Call site: `crates/ec-av1/src/decode.rs:37668` (`read_intra_chroma_lossless`).
The sibling walk in `decode_block` (`decode.rs:22476`) already used `tu_reach_rect`
with separate `luma_span_x`/`luma_span_y`; `read_intra_chroma_lossless` did not.

### Why interior-only

`build_directional_and_filter_intra_predictors` (`reconintra.c:1150`) copies
`n_left_px` samples into `left_col[0..3]`, then **only if `n_bottomleft_px > 0`**
copies the next `n_bottomleft_px` into `left_col[4..7]`. With the reach denied,
our `Edges::build` (`intra.rs:187`) replicated `left_col[3]` over indices 4..7.
`av1_dr_prediction_z3_c` reads `left_col[base]`/`[base+1]` with `base = (23*(c+1)+r) >> 6`:

- row 0 / col 0 of the block only ever read `left_col[0..3]` → **byte-identical**
- the interior (and `col0[3]`) read `left_col[4..7]` → the replicated tail

The below-left edge supplies only the **base** term (the DC of the directional
walk); the angle interpolation term is unchanged. That is precisely an
interior-only difference with matching row0/col0 and matching coefficients.

First wrong unit (paired by decode order, 9600 records each side, pairing verified
unambiguous — coordinate agreement exact except a uniform −2 reporting artifact in
libaom's `xd->mi_col` on odd columns):

```
mi(18,30)  plane=V  chroma (60,72)  4x4  mode=7 (D203_PRED)  angle_delta=-1  p_angle=200
ours  pred=[158,158,157,156, 156,156,155,154, 154,154,153,153, 153,153,153,153] sum=2476
oracle pred sum=2468   row0 SAME  col0 ours=[158,156,154,153] oracle=[158,156,154,152]
oracle n_top=4 n_left=4 n_tr=-1 n_bl=4 bsize=3(BLOCK_8X8) part=0
our fed left slice (EC_DEBUG_EDGES): Some([158,157,155,153])   <-- 4, not 8
```
Feeding libaom's own `z3` formula the 8-sample left column
`[158,157,155,153,151,149,146,143]` reproduces the oracle's col0 `[158,156,154,152]`
exactly; our predictor arithmetic was already correct — only the edge was short.
Residual was byte-identical (`res0=[-1,-3,-4,-5]` both sides).

## Measurements

`ll422_allintra` (per plane, vs `aomdec --rawvideo`):

| cell | plane | before | after |
|---|---|---|---|
| ll422_allintra | Y | 0 | **0** |
| ll422_allintra | U | 3126 | **2984** (−142) |
| ll422_allintra | V | 3139 | **3067** (−72) |

First wrong U sample moved from rel 1853 to rel 5222; the earlier divergence class
is gone. 490 → 453 prediction-sum disagreements out of 9600 paired units.

**Oracle-flip control:** one byte of the oracle V plane flipped
(offset 126780, 157→158) → V count moved **3067 → 3068, exactly +1**. The
comparator is live.

**Regression** (W_intrabc / X_intrabc_tiled, 16 frames each, totals over all frames):

| cell | plane | before | after |
|---|---|---|---|
| W_intrabc | Y / U / V | 917974 / 474506 / 487694 | **917974 / 474506 / 487694** (no change) |
| X_intrabc_tiled | Y / U / V | 864325 / 421718 / 414871 | **864325 / 421718 / 414871** (no change) |

Unchanged because both are inter/intrabc streams; the changed function is reached
only from intra lossless 4:2:2.

`cargo test -p ec-av1 --lib -- 422 lossless 444 warp --skip bitrate_target_lands_within_5_percent_over_48_frames`
→ **90 passed, 0 failed** (with the 4:2:2 header guard in place).
(Run with the bypass patched in, 5 `…_refuse_by_name` tests fail — that is the
bypass disabling the refusal, not a regression.)

## Gating — this change is NOT gateable

**Stated plainly:** 4:2:2 is refused at the sequence header
(`crates/ec-av1/src/stream.rs:1803`, `if seq.subsampling_x != seq.subsampling_y`).
No committed test build can reach `read_intra_chroma_lossless` at ss (1,0), so
**there is no committed test that can gate this fix.** The evidence is the
measurement table above plus the oracle-flip isolation control, not a test.
Per instruction, no source-scan gate was substituted and this change is **not**
described as gated. The 4:2:2 refuse-vs-lift product decision is untouched.

## Remaining residual — identified, not fixed

Residual drops 3126→2984 / 3139→3067, so this is one member of a family. The next
first divergence after the fix:

```
idx 2818  mi(8,49)  plane=U  chroma (96,32)  4x4  mode=7 (D203_PRED)  ad=0  p_angle=203
ours sum=2656  oracle sum=2653   row0 SAME, col0 SAME  -> interior-only again
oracle: n_top=4 n_left=4 n_tr=-1 n_bl=4 bsize=2 (BLOCK_8X4)
```
`bsize=2` is **BLOCK_8X4** — a rectangular luma leaf. `read_intra_chroma_lossless`
still passes `side, side` as the reach's `(bw, bh)`, so for a rect leaf `bh` is the
leaf's WIDTH, not its height; libaom's `has_bottom_left` uses
`mi_size_high[bsize]` (`reconintra.c:410`). For mi(8,49) libaom returns 1 through
the **superblock leftmost-column** branch (`reconintra.c:421-434`):
`blk_col_in_sb == 0` → `row_off_in_sb + 1 < 16` → true, a branch
`Reach::of_tu` does not model at all. Our `left` slice is still 4 samples there.

Two distinct follow-ups, both in the same function:
1. pass the rect leaf's true luma `(bw, bh)` instead of `side, side`;
2. port `has_bottom_left`'s `blk_col_in_sb == 0` superblock branch into
   `Reach::of_tu` (currently only the `blk_row` bottom-row and table cases exist).

Neither is fixable by the one-line per-axis change; both need the rect-leaf shape
and the superblock leftmost-column predicate.

## Provenance

- worktree `/home/tahinli/.cache/wt/av1422llpred`, branch `lane-av1422llpred`, from `main` = `2005a35d`
- `CARGO_TARGET_DIR=/home/tahinli/.cache/tgt/av1422llpred`
- oracle: instrumented `/home/tahinli/.cache/aom-oracle/build/aomdec`, `EC_PRED` rung
  (`reconintra.c:1868`), picture counter `aom_ec_pict_idx` (`decodeframe.c:89`)
- rungs used: ours `EC_PRED` / `EC_DEBUG_EDGES`; oracle `EC_PRED` / `EC_PREDOUT8`
- bypass patch (`stream.rs` `if false && …`) applied for probing only and
  **restored**; the only committed-tree change is the 1-line `decode.rs` fix
