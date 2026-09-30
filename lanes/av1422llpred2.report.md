# lane-av1422llpred2 — 4:2:2 sub-8 chroma reach: the chroma plane's block shape is the PLANE-scaled one

Target: `probe/ll422_allintra.obu` (320x240, 4:2:2 lossless all-intra, 1 frame).
Base: `main` = `d202b8b4` (the dc_sign, per-axis-reach and per-plane-warp fixes
already landed). Worktree `/home/tahinli/.cache/wt/av1422llpred2`, branch
`lane-av1422llpred2`, `CARGO_TARGET_DIR=/home/tahinli/.cache/tgt/av1422llpred2`.

**Result: `ll422_allintra.obu` is now BYTE-EXACT against `aomdec --rawvideo` —
Y 0 / U 0 / V 0** (from Y 0 / U 2984 / V 3067). 4:2:0 and 4:4:4 stay byte-exact.

## 1. Re-measure on my tip (before touching anything)

Reproduced the handoff numbers exactly, with a live comparator:

| cell | plane | before | after | oracle-flip control |
|---|---|---|---|---|
| `ll422_allintra` (4:2:2) | Y | 0 | **0** | — |
| | U | **2984** | **0** | one byte flipped → U +1 |
| | V | **3067** | **0** | one byte flipped (offset 126780) → V 3067→**3068**, i.e. +1 |
| `ll420_allintra` (4:2:0) | Y/U/V | 0/0/0 | **0/0/0** | — |
| `ll444_allintra` (4:4:4) | Y/U/V | 0/0/0 | **0/0/0** | — |
| `W_intrabc` (16 frames) | Y/U/V | 807779/389623/418698 | **807779/389623/418698** (delta 0) | one byte flipped → +1 on every plane |
| `X_intrabc_tiled` (16 frames) | Y/U/V | 761090/361159/386148 | **761090/361159/386148** (delta 0) | one byte flipped → +1 on every plane |

`W_intrabc` / `X_intrabc_tiled` are pre-existing, wholesale-divergent cells (their
FIRST sample already differs, so they are not in this defect's class at all);
their deltas are exactly zero and the flip controls prove the comparator reads
both sides.

Prediction-sum disagreements on `ll422_allintra`: **453 of 9600** paired units
before → **0 of 9600** after.

## 2. The first wrong chroma unit, in decode order

Pairing is 1:1 in decode order (9600 records each side), coordinates verified
against the oracle's own `EC_PRED`/`EC_PREDOUT8`. The first wrong unit:

```
idx 2818  plane=U(1)  chroma (96,32)  4x4  mode=7 (D203_PRED)  ad=0  p_angle=203
ours  sum=2656  row0=[166,166,166,166]  col0=[166,166,166,166]
oracle sum=2653  row0=[166,166,166,166]  col0=[166,166,166,166]   <-- row0 AND col0 byte-identical
oracle EC_PRED mi_row=8 mi_col=49 plane=1 row_off=0 col_off=0 txw=4 txh=4
             mode=7 p_angle=203 have_top=1 have_left=1
             n_top=4 n_left=4 n_tr=-1 n_bl=4 bsize=2 part=0 ft=0
```

Neighbour availability **disagreed**: the oracle reports `n_left=4 n_bl=4`
(8 left-column samples), our side fed **4**:

```
our EC_DEBUG_EDGES, this unit:
  EDGES_SQ x=96 y=32 side=4 mode=7 above=Some([166,167,166,166,166,166,166,166])
                             left=Some([166,166,166,166])     <-- 4, not 8
                             reach=Reach { above_right: true, below_left: false }
```

Both plane EDGES of the same shape are byte-identical (above-right agrees);
only the below-left tail of the left column is missing.

## 3. The named arithmetic

The oracle's `bsize=2` is the first clue: libaom does **not** answer a chroma
plane's reach with the luma block's size.

| | expression | value at unit 2818 |
|---|---|---|
| libaom, shape | `bsize = scale_chroma_bsize(mbmi->bsize, ss_x, ss_y)` — `reconintra.c:1823`, definition `reconintra.c:1649` | leaf is BLOCK_4X4, `ss (1,0)` → `bs = BLOCK_8X4` = **2 mi wide, 1 mi tall** (`mi_size_wide[BLOCK_8X4]=2`, `mi_size_high[BLOCK_8X4]=1`) |
| libaom, in-block early return | `row_off + tx_size_high_unit[TX_4X4] < AOMMAX(mi_size_high[bsize] >> ss_y, 1)` — `reconintra.c:412-417` | `0 + 1 < AOMMAX(1>>0, 1) = 1` → **false** |
| libaom, leftmost-SB-column arm | `blk_col_in_sb = (mi_col & (sb_mi_size-1)) >> bw_in_mi_log2` — `reconintra.c:421`; then `row_off_in_sb + count < sb_height_unit` — `reconintra.c:429-435` | `(49 & 15) >> 1 = 0` → arm taken; `(8<<0)>>0 + 0 + 1 < 16>>0` = `9 < 16` → **1** ⇒ `n_bl = AOMMIN(txhpx, yd) = 4` |
| ours (was) | `Reach::of(4, px, py, …)` — `crates/ec-av1/src/encode.rs`, `Reach::of` → `bottom_left(4, …)` → `has_bl_4x4` lookup: `position(4, 196, 32)` = `((32%64)/4, (196%64)/4, 16)` = `(8, 1, 16)`; index `row*32 + col = 257` → `HAS_BOTTOM_LEFT[3][32] = 84 = 0b0101_0100`, bit 1 clear | `col != 0`, `row+1 != 16` → table read → **false** |
| ours (now) | `Reach::of_tu_chroma(mi_bw, mi_bh, mi_row, mi_col, sb_mi, col_off, row_off, tx_w, tx_h, ss_x, ss_y, above_avail, below_avail)` — `crates/ec-av1/src/encode.rs`, a transcription of `has_top_right`/`has_bottom_left` | `bw_log2 = 1`, `bh_log2 = 0`; `blk_col = (49&15)>>1 = 0`; `from_block = 8`, `sb_height = 16-8 = 8`; `0 + 4 < 8<<2 = 32` → **true** |
| call site | `crates/ec-av1/src/decode.rs`, `sub8_leaf_chroma422` (the 4:2:2 sub-8 leaf's own chroma TX_4X4 reader) | `reach = Reach::of_tu_chroma(chroma_mi_shape(leaf_shape.0, leaf_shape.1, ss_x, ss_y), …)` |

`scale_chroma_bsize` is transcribed in `crates/ec-av1/src/encode.rs` as
`chroma_mi_shape` (the only remapped family is BLOCK_4X4 / BLOCK_4X8 /
BLOCK_8X4 / BLOCK_4X16 / BLOCK_16X4; everything else passes through).

### Why it yields exactly an interior-only difference

`build_directional_and_filter_intra_predictors` (`reconintra.c:1150-1158`):

```c
const int num_left_pixels_needed = txhpx + (n_bottomleft_px >= 0 ? txwpx : 0);
i = 0;
if (n_left_px > 0) {
  for (; i < n_left_px; i++) left_col[i] = left_ref[i * ref_stride];
  if (n_bottomleft_px > 0) {                       /* <-- only here */
    assert(i == txhpx);
    for (; i < txhpx + n_bottomleft_px; i++) left_col[i] = left_ref[i * ref_stride];
  }
  if (i < num_left_pixels_needed)
    memset(&left_col[i], left_col[i - 1], num_left_pixels_needed - i);   /* replicate */
}
```

With `n_bottomleft_px = 4`, `left_col[0..3]` = the four left samples and
`left_col[4..7]` = the four below-left samples. With `n_bottomleft_px = -1`
(our denial) `num_left_pixels_needed = txhpx = 4`, so `left_col[4..7]` is never
written by the copy at all and the directional kernel reads whatever follows —
the block's own (not yet reconstructed) rows.

`av1_dr_prediction_z3_c` (`reconintra.c:609`) reads
`left_col[base]`/`left_col[base+1]` with `base = (dr_intra_derivative[70] * (c+1) + r) >> 6`, `dr_intra_derivative[70] = 23` (`reconintra.h:81`/`:122`). At `p_angle = 203` (`need_left`, `need_above = 0`) row 0 of the block never reads `left_col` at all, and column 0 reads only the `base` values that land on `left_col[0..3]`. So:

* **row 0 identical** — it is `above`-driven, and `above_right` already agreed.
* **col 0 identical** — it reads only the four samples libaom and we share.
* **interior differs** — `base` walks up to `left_col[7]`, i.e. the below-left
  tail, which is precisely what the denial removed.
* **coefficients bit-identical** — the transform read is untouched by any of this,
  so the residual adds the same numbers to a different base.

That is the whole signature: edges and row0/col0 equal, interior differs, luma
exact, coefficient stream paired with zero disagreement.

### Is it 4:2:2-specific or merely 4:2:2-reachable?

`scale_chroma_bsize`'s remap table is not 4:2:2-only — at 4:2:0 a BLOCK_4X4
leaf is also answered as BLOCK_8X8. But **the class is unreachable at 4:2:0 and
4:4:4 in this decoder**, for two independent reasons, and both are measured:

1. The fixed reader `sub8_leaf_chroma422` is 4:2:2-only by construction; its
   siblings are `sub8_leaf_chroma444` (4:4:4) and the `chroma_444 || chroma_422
   && lossless` arm at `decode.rs`. No 4:2:0 path calls it.
2. `Reach::of_tu_chroma` is called from exactly one place, in a function whose
   name and dispatch say 4:2:2.

At 4:2:2 the remap is what makes the defect visible: BLOCK_4X4 → BLOCK_8X4 puts
`blk_col_in_sb` at 0 (mi columns 0–1) where the one-mi-wide luma leaf reads
column 1, and the per-block table says 0 there. At 4:2:0 the same remap gives
BLOCK_8X8, whose `blk_col_in_sb = (mi_col & 15) >> 1` selects the SAME arm our
`col == 0` test selects for an even-column leaf, so the branch choice agrees;
only the arm's arithmetic differs (`blk_start_row_off = (blk_row << 1) >> 1`
vs our `row * side + side < sb_px`), and no committed 4:2:0 fixture reaches it
— measured: `ll420_allintra` stays 0/0/0 and the whole 4:2:0/4:4:4 suite is
unchanged (see §5). **That 4:2:0 arithmetic difference is a LATENT instance of
this class, unexercised and NOT fixed by this lane. It is reported, not
claimed fixed.**

## 4. The change

* `crates/ec-av1/src/encode.rs`
  * `Reach::of_tu_chroma(...)` — a transcription of libaom's own
    PLANE-scaled block shape, including the arm `Reach::of_tu` never had: the
    superblock's leftmost-column / top-row tests. Takes the two
    `*_available` early returns as arguments because libaom states them as
    TILE bounds over the TRANSFORM's plane extent.
  * `Reach::plane_reach_tables(mi_bw, mi_bh)` — the `has_tr_*`/`has_bl_*` pair
    for a block shape that can be a rectangle where `Reach::table` would read
    a square's row.
  * `chroma_mi_shape(bw, bh, ss_x, ss_y)` and `reach_sb_mi(fctx)` —
    `scale_chroma_bsize` and `sb_mi_size` in mi units.
* `crates/ec-av1/src/decode.rs` — `sub8_leaf_chroma422` computes the leaf's
  chroma reach through `of_tu_chroma` at `scale_chroma_bsize`'s shape, instead
  of `Reach::of(4, …)` / `Reach::of_rect(leaf_shape, …)`.

`read_intra_chroma_lossless` was tried first and REVERTED: its `side` is ≥ 8 on
every call site, and the remap is the identity for a block of 8 or more at every
subsampling, so it changed nothing (measured 2984/3067 unchanged) while adding
an unjustified behaviour change to a 4:2:0-hot path.

## 5. Regression

```
cargo test -p ec-av1 --lib -- 422 lossless 444 warp intra \
  --skip bitrate_target_lands_within_5_percent_over_48_frames
```

```
test result: ok. 175 passed; 0 failed; 2 ignored; 0 measured; 622 filtered out;
finished in 276.74s
```

The FIRST run of this same command was made with the `stream.rs` bypass patch
still applied and reported **170 passed / 5 failed** — all five failures were
`stream::tests::the_pinned_422_*_refuses_by_name`, i.e. the 4:2:2 REFUSAL gates
failing because the bypass was in the tree, not a regression from this change.
With `stream.rs` restored (`git checkout --`) the suite is fully green.

### Bypass hygiene, re-verified

* `git diff --stat` on the tip is `crates/ec-av1/src/decode.rs` and
  `crates/ec-av1/src/encode.rs` only. `stream.rs` is UNPATCHED now.
* No code reads `EC_AV1_ALLOW_422_PROBE` anywhere under `crates/` — the only
  hits are doc comments naming the recipe. Verified with a grep for
  `env_flag!("EC_AV1_ALLOW_422_PROBE")` / `var("EC_AV1_ALLOW_422_PROBE")`:
  zero hits.
* The refusal string `a chroma format of 4:2:2` is byte-identical (8
  occurrences, unchanged), and the five `the_pinned_422_*_refuses_by_name`
  gates pass.

To reproduce the 4:2:2 numbers, re-apply the one-line patch
(`if false && seq.subsampling_x != seq.subsampling_y`) and restore it after.

## 6. Gating — stated plainly

**This change cannot be gated by any committed test.** 4:2:2 is refused at the
sequence header (`stream.rs`: `a chroma format of 4:2:2 (subsampling_x !=
subsampling_y)…`), so `sub8_leaf_chroma422` is unreachable from any stream the
committed suite can decode, and no gate could ever run it. Per the lane's
instruction no source-scan gate was substituted and this change is **not**
described as gated. The evidence is the measurement table in §1 — before/after
per plane per cell, each with an oracle-flip isolation control that moves the
count by exactly +1 — plus the 4:2:0 / 4:4:4 identity measurements and the
regression run in §5.

What is *not* claimed: that the residual class is exhausted. `ll422_allintra` is
byte-exact and its 9600 prediction sums all agree; whether another 4:2:2 stream
exercises a shape this lane did not reach is not established.

## 7. Rung label spaces used

`skill://av1-coeff-ladder-shape` applied. Ours `EC_PRED`/`OUR_PRED`
(`decode.rs`, `x`/`y` are this PLANE's pixel origin) and the `EC_DEBUG_EDGES`
rung; the oracle `EC_PRED` (directional, prints the LOCAL `mi_row`/`mi_col`
plus `n_top`/`n_left`/`n_tr`/`n_bl`/`bsize`) and `EC_PREDOUT8` (sum + row0/col0).
The oracle's `EC_PREDOUT8` non-directional rung prints `xd->mi_*` while its
`EC_PRED` prints `xd->mi_*` restated through `mb_to_*_edge`; for chroma the
leaf libaom decodes is the ODD-column chroma reference
(`is_chroma_reference`, `av1_common_int.h:1454`) while the plane origin is
snapped back to the even column by `setup_pred_plane`'s
`if (subsampling_x && (mi_col & 0x01) && (mi_size_wide[bsize] == 1))
mi_col -= 1;` (`reconinter.h:392-393`) — so `mi_col=49` in the rung and chroma x
96 on the plane are BOTH correct, and the placement was verified identical
before the reach was blamed (the two U planes agree byte-for-byte across
chroma x 96..99 at the failing rows).

## 8. Provenance

- worktree `/home/tahinli/.cache/wt/av1422llpred2`, branch
  `lane-av1422llpred2`, from `main` = `d202b8b4`
- `CARGO_TARGET_DIR=/home/tahinli/.cache/tgt/av1422llpred2`, `EC_NOMEMGUARD=1`
- oracle: instrumented `/home/tahinli/.cache/aom-oracle/build/aomdec`, rungs
  `EC_PRED` (`reconintra.c:1859`) and `EC_PREDOUT8` (`reconintra.c:1803`)
- cells: `/home/tahinli/.cache/cells/av1422lpf/{probe,W_intrabc,X_intrabc_tiled}`
- no push, no merge, no rustfmt
