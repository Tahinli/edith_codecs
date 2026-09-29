# lane-av1loss444kf — the 4:4:4-lossless key frame's 49 chroma samples: found, fixed, witnessed

**Tree.** branch `lane-av1loss444kf` off `c6112723`, worktree
`~/.cache/wt/av1loss444kf`, `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-av1loss444kf`
(lane-private), `TMPDIR=$HOME/.cache/tmp`. Nothing pushed. Files touched:
`crates/ec-av1/src/decode.rs` (the fix + the counter + its accessor),
`crates/ec-av1/src/stream.rs` (one comparison helper + one gate). No temporary
instrumentation survived — see §7.

## 0. Verdict in one table

| question | measured answer |
|---|---|
| does the carrier reproduce? | **yes**, byte-for-byte: 109215 B, sha256 `4563a01f17786000…` |
| does the 49-sample fingerprint reproduce? | **yes**, exactly: 21 U + 28 V, same extents, same per-cell counts 6/16 · 9/16 · 6/16, first sample U(201,200) 160 vs 159 |
| which stages are inert? | the oracle's pre-deblock, post-deblock, post-CDEF and final key-frame dumps are **byte-identical to each other**; ours too. The whole filter chain is inert on BOTH sides |
| prediction or residual? | **prediction**, and arithmetically so: the pixel delta IS the prediction delta (identical coefficients ⇒ identical residual), and it is fully reproduced by libaom's z3 with a different left edge |
| fixed? | **yes** — 49 → **0**, on the 109215 B carrier *and* on the 109909 B one |
| witness | `a_lossless_444_sub8_rect_leaf_chroma_reach_decodes_the_key_frame_pixel_exact`, red-before at the exact 49/first-sample fingerprint, non-vacuous on a counter that reads **86** on this cell |
| 4:2:0 / 4:2:2 identity | structural: every caller of the changed function sits behind `chroma_444` (ss 0/0). `-- lossless 444` 48/0, `-- 420` 2/0, `-- intrabc` 21/0, `-- 422` 6/0 |

## 1. The carrier, pinned and reproduced

`testsrc2 256x256:rate=25` → `yuv444p`, 6 frames, piped to the durable oracle as
y4m, encoded with

```
aomenc --lossless=1 --enable-palette=0 --enable-intrabc=0 \
       --codec=av1 --passes=1 --end-usage=q --threads=1 --row-mt=0 \
       --lag-in-frames=0 --kf-max-dist=100 --limit=6 --obu -o - -
```

* **109215 bytes, sha256 `4563a01f1778600020ffa9f15bc05f5559321cf5712b99a29ed7192675dee6a4`,
  fnv1a64 `0xbbb56442f393a9b2`** — the cell `lanes/av1tilemeasure` published. The
  gate re-encodes it live and asserts BOTH the length and the fnv1a64, so a
  recipe or encoder drift fails as drift instead of passing as this cell.
* The key frame alone, split to `SEQ + TD + FRAME` in the original OBU order
  (`split.py`, 18140 B, sha256 `f390434237f1a94f…`), is what every measurement
  below runs on: `aomdec --limit=1` and this decoder each see one frame, so no
  inter-frame walk defect can be mistaken for this one.
* The second, independent encode of the same cell — `+ --sb-size=128` with
  intrabc on, **109909 B, sha256 `d4427a7372e443e1…`** (key frame 18131 B) — is
  the second arm in §5.

**Fingerprint, measured on the key frame (ours vs `aomdec`'s `EC_AV1_FINAL_DUMP`):**

| | before | after |
|---|---|---|
| key frame samples differing from the oracle | **49** | **0** |
| per plane | U 21, V 28, **luma 0** | 0 / 0 / 0 |
| extent | U x 196..207 y 200..203, V x 196..206 y 200..203 | — |
| per 4x4 cell (U) | 6/16, 9/16, 6/16 | — |
| first sample | plane 1, x 201, y 200: ours 160, oracle 159 | — |
| deltas (ours − oracle) | U **−3 … +1**, V **−6 … +1** | — |

**One correction to the handed-down fingerprint, measured.** The report I was
given says the deltas run `+1 … +63` (U) and `−54 … +47` (V), 17 and 23 distinct
values. On the pinned 109215 B carrier the deltas are **−3 … +1** (U, 4 distinct)
and **−6 … +1** (V, 6 distinct). The COUNT, the planes, the extents, the per-cell
partiality and the first sample all reproduce exactly; only the magnitude claim
does not, and it was read off a stream that is not this one. The "not a rounding
constant" conclusion survives (it is not a constant), but nothing in this
defect needs ±63.

## 2. Stage ladder: the whole filter chain is inert, on BOTH sides

One `aomdec` run of the key-frame stream with four dump rungs at once
(`EC_AV1_PREFILT_DUMP`, `EC_AV1_POSTDEBLOCK_DUMP`, `EC_AV1_POSTCDEF_DUMP`,
`EC_AV1_FINAL_DUMP`), and this decoder with its own `EC_AV1_PREFILT_DUMP`:

```
oracle  pre == deblock == cdef == final      (all four files byte-identical, 196608 B)
ours    pre == final                         (byte-identical)
ours pre vs oracle pre                      49 samples — the SAME 49
```

So deblocking, CDEF and loop restoration changed nothing on either side of this
frame, and the divergence is already present in the **reconstruction**. The
filter chain is exonerated as the ORIGIN, not merely as a contributor, and that
is measured here rather than inherited from the other lanes' stage claims.

## 3. Prediction vs residual: decided by arithmetic, then by transcription

**The cheap discriminator.** On a lossless frame `recon = prediction + residual`
exactly, and the coefficients are the same on both sides (entropy is bit-locked,
the transform-unit walk is position-identical), so the residual is the same on
both sides. Therefore

> the 49-sample pixel delta **is** the 49-sample prediction delta.

That alone moves the search space from "prediction, dequant/WHT, placement" to
"prediction" — but it is only valid if the coefficients really are identical, so
it was checked rather than assumed: with the prediction delta fully explained
(§3b) the coefficients cannot differ anywhere, because a coefficient difference
would show up as a pixel delta that the prediction cannot produce.

**The paired prediction rungs.** `EC_PRED=1` on both decoders over the key frame:

* the oracle prints 3130 `EC_PRED` lines (directional + filter-intra only; a
  non-directional block prints just `EC_PREDOUT8`) and **12288** `EC_PREDOUT8`
  lines; this decoder prints **12288** `OUR_PRED` lines. Same count, same
  decode order.
* Index-pairing is a trap here: at the first order disagreement our (200,196)
  is read against the oracle's (196,200) and every value looks wrong. Pairing
  by `(x, y, plane)` instead — both traces carry the coordinates, the oracle's
  from `mb_to_top_edge` — gives **exactly 4 sum mismatches out of 3130
  directional predictions**, all four inside the two 4x8 rect leaves at
  (196,200) and (200,200), chroma planes, mode 7 (D203, p_angle 203). Every
  other directional prediction in the frame, luma and chroma, is identical.
* The third damaged block (204,200) is a `UV_CFL_PRED` leaf, whose base
  prediction is DC and therefore prints no `EC_PRED` line at all. It is damaged
  for a different reason — see §3c.

**3b. The 16 samples, from a transcription of libaom's own code.** With the
prediction deltas known, `pred_oracle = pred_ours − (recon_ours − recon_oracle)`
gives the oracle's full 4x4 prediction for each damaged unit. A from-scratch
transcription of `av1_dr_prediction_z3_c` + `av1_filter_intra_edge_c` +
`av1_upsample_intra_edge_c` + `intra_edge_filter_strength` +
`av1_use_intra_edge_upsample` (`reconintra.c:573-638, 989-1082`), fed the edge
pixels read out of the ORACLE's own reconstruction, then run over the two
candidate left columns:

| unit | n_bl = 0 (replicated edge) | n_bl = 4 (real bottom-left) |
|---|---|---|
| (196,200) U | **= our prediction, all 16** | = the oracle's prediction, all 16 |
| (196,200) V | = our prediction, all 16 | = the oracle's prediction, all 16 |
| (200,200) U | neither | **= the oracle's prediction, all 16** |
| (200,200) V | neither | **= the oracle's prediction, all 16** |

16/16 samples on all four units, with the intra edge filter type at both values
(ft=0 and ft=1 make no difference at p_angle 203, 4x4: strength 0 either way and
upsample 1 either way — so the filter type is NOT the variable here, and the
`ft` column in the oracle's trace is a red herring for this cell). **The
difference is exactly `n_bottomleft_px`, and nothing else.** The dequant scale,
the 4x4 WHT and the unit placement are exonerated by construction: they produce
the residual, and the residual is not what differs.

**3c. Why two different predictors looked guilty.** Blocks #735 (D203) and #736
(CFL) do not fail independently. On a lossless frame a wrong prediction is
written to the frame and the NEXT block reads it as its left edge, so the
damage cascades one block to the right: our wrong pixels at (199, 201) and
(199, 202) are the left column of the block at (200,200) (measured: oracle
`[158,164,169,166,…]`, ours `[158,165,167,166,…]`), and the pixels at
(203, 201/202) are the left column of the CFL block. **One wrong block explains
all three** — the D203+CFL mix was a consequence, not a second cause. This is
what killed the "two predictors, one cause" reading the defect invited.

## 4. The deciding site, and the fix

`crates/ec-av1/src/decode.rs`, `sub8_leaf_chroma444` — the 4:4:4 lossless chroma
walk of a sub-8x8 leaf. Each TX_4X4 chroma unit asked for its reach with

```rust
let unit_reach = Reach::of(4, upx, upy, y.width, y.height, fctx);
```

which is a **standalone `BLOCK_4X4` lookup at the unit's own position**. libaom
(`reconintra.c:1852`) asks `has_bottom_left(sb_size, bsize, mi_row, mi_col, …,
txsz, row_off, col_off, ss_x, ss_y)` — with the **leaf's** `bsize`, the unit's
`(row_off, col_off)` and `tx_size` — and that function's first branch inside the
left column is

```c
const int bh_unit = mi_size_high[bsize];
const int plane_bh_unit = AOMMAX(bh_unit >> ss_y, 1);
const int bottom_left_count_unit = tx_size_high_unit[txsz];
if (row_off + bottom_left_count_unit < plane_bh_unit) return 1;   // the block's OWN next row
```

For a 4x8 leaf's FIRST unit (`row_off = 0`, `tx = TX_4X4`) that is `0 + 1 < 2` →
**true**: the four bottom-left samples are the block's own second-row left
neighbours, already reconstructed. The oracle's own `EC_PRED` rung prints
`n_bl=4` there and `n_bl=0` for the leaf's second unit, exactly as that ladder
predicts. Our 4x4 table said false, so `PlaneBuf::edges` handed the predictor a
left column replicated out to the edge's full `bw + bh` reach, and the unit
predicted from a fabricated edge.

`Reach::of_tu` is the crate's existing transcription of that pair of functions
(`has_tr_*`/`has_bl_*` for a unit at `(col_off, row_off)` inside a `bw x bh`
block whose own answer is `block`), and it is what the function's OWN block-level
`reach` is built for — the value was computed at the top of `sub8_leaf_chroma444`
and then not used by the per-unit walk. The 4:2:2 twin of this function was
fixed exactly this way already (lane-av1-422kf2, whose comment at `decode.rs`
names "the standalone square-4x4 table the old `Reach::of(4, ..)` lookup used");
this was its 4:4:4 twin, still carrying the stale copy.

```rust
let unit_reach = Reach::of_tu(bw, bh, uc * 4, ur * 4, 4, 4, reach);
if !square && unit_reach != Reach::of(4, upx, upy, y.width, y.height, fctx) {
    hit!(SUB8_CHROMA444_LOSSLESS_TU_REACH_HITS);
}
```

applied at **both** lossless 4:4:4 arms (the intra one and the intrabc one), plus
the counter declaration and its two accessors.

**The class, swept repo-wide.** Every `Reach::of(4, …)` in `decode.rs` is now
accounted for, and this was the last stale copy:

| site | verdict |
|---|---|
| `sub8_leaf_chroma444` block-level reach (~23942) | correct — the leaf's own answer, the value the fix now threads down |
| `sub8_leaf_chroma444` per-unit, intra arm | **fixed** |
| `sub8_leaf_chroma444` per-unit, intrabc arm | **fixed** (same lines, the same defect) |
| `sub8_leaf_chroma422` block-level reach | already correct (lane-av1-422kf2) |
| `decode_leaf_split4` (~25016) | correct — a standalone `BLOCK_4X4` leaf really is a 4x4 block |
| `read_intra_chroma_lossless`'s `tu_reach(side, side, …)` | correct — all four of its callers pass a SQUARE plane block, so the unit dims are the block dims |

`Reach::of_tu` itself is **unchanged**; the fix only starts calling it. Nothing in
`encode.rs`, `intra.rs` or `transform.rs` is touched.

## 5. The witness, and the red-before

`stream::tests::a_lossless_444_sub8_rect_leaf_chroma_reach_decodes_the_key_frame_pixel_exact`
with a new non-panicking helper `key_frame_diffs` (`aomdec --limit=1` +
`EC_AV1_FINAL_DUMP` on frame 0 only, versus this decoder's own decode-order
frame 0), because a whole-stream byte-exactness assert cannot name this defect:
the carrier's inter frames still carry the SEPARATE mu-chunk chroma-walk defect
(§6).

Non-vacuity, in order, every step load-bearing:

1. the live re-encode is 109215 bytes and fnv1a64 `0xbbb56442f393a9b2` — a
   drifted recipe/encoder fails HERE, not silently;
2. `assert_444_header` — at 4:2:0 the changed function is unreachable and every
   assert below would pass for free;
3. the key frame is byte-exact, 0 of 196608 samples, and the failure message
   NAMES the first differing sample as `(plane, x, y, ours, oracle)`;
4. `sub8_chroma444_lossless_tu_reach_hits()` rose over the decode — measured
   **86** units on this cell, i.e. 86 units where the block-relative answer and
   the standalone lookup disagree. This is what makes the gate a witness of the
   FIXED path rather than of a stream that merely contains rect leaves.

**Red-before, with only the two reach expressions reverted** (counter, accessor
and gate untouched):

```
assertion `left == right` failed: the 4:4:4 lossless key frame differs from
aomdec in 49 samples (first Some((1, 201, 200, 160, 159)) as
(plane, x, y, ours, oracle))
  left: 49
 right: 0
test result: FAILED. 0 passed; 1 failed
```

Restored → green:

```
lossless-444-sub8-rect-leaf-chroma-reach: key frame byte-exact vs aomdec
(0 of 196608 samples differ), 86 rect-leaf chroma unit(s) took the
block-relative reach
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 748 filtered out
```

**Second, independent encode.** The 109909 B stream (`--sb-size=128`, intrabc
ON, a different key frame with 82697 symbol reads) decodes its key frame
**byte-exactly** with the fix and with 49-ish samples wrong without it. Same
defect, same class, two byte streams.

**The 49-sample tripwire.** No bound anywhere was re-pinned: the tripwire is the
gate's own `differing == 0`, and it came DOWN to 0 as a consequence of the fix.
Nothing was weakened to make a gate pass.

## 6. Identity at other shapes

| set | result |
|---|---|
| `cargo test -p ec-av1 --lib -- lossless 444` | **48 passed, 0 failed** (17.4 s) |
| `cargo test -p ec-av1 --lib -- 420` | **2 passed, 0 failed** |
| `cargo test -p ec-av1 --lib -- intrabc` | **21 passed, 0 failed, 1 ignored** |
| `cargo test -p ec-av1 --lib -- 422` | **6 passed, 0 failed** (all refusal-inventory: 4:2:2 is refused by name) |

Arithmetically first, then measured:

* **4:2:0 and 4:2:2 are structurally unreachable.** All five call sites of
  `sub8_leaf_chroma444` sit behind `if chroma_444`, which is
  `ss_x(fctx) == 0 && ss_y(fctx) == 0`. The changed lines cannot execute at
  either subsampling, so the 4:2:0 and 4:2:2 decodes are unchanged by
  construction, not by measurement alone.
* **A 4x4 (square) leaf is unchanged even at 4:4:4.** `of_tu(4, 4, 0, 0, 4, 4, r)`
  has `col_off == 0` (so not the `col_off > 0 → false` branch) and
  `row_off + tx_h < bh` is `0 + 4 < 4` — false — so BOTH flags fall through to
  `r`, the leaf's own reach, which for a square leaf is
  `Reach::of(4, px, py, ..)`: the old value exactly. The counter's `!square`
  guard is that same fact, so a square leaf cannot even bump it.
* The rect leaves the fix does move are the `(8, 4)` / `(4, 8)` shapes the three
  non-square call sites pass; a `4x4` leaf is the only square one.

## 7. What is still open, precisely

* **The carrier's inter frames still diverge, and that is NOT this defect.**
  Frames 1..5 still differ, and they differ by almost exactly as much as they did
  BEFORE this fix (same 6-frame stream, same tree, only the two reach
  expressions flipped):

  | decode-order frame | before | after |
  |---|---|---|
  | 0 (KEY) | 49 | **0** |
  | 1 | 70151 | 70149 |
  | 2 | 138579 | 138579 |
  | 3 | 114966 | 114962 |
  | 4 | 139220 | 139220 |
  | 5 | 139919 | 139918 |

  That residual is the per-mu-chunk lossless chroma walk
  (`lanes/av1loss444mm.report.md` r1: the intra tail of `decode_inter_block`'s
  `side > 64` arm passed the chunk's chroma region as `(cu_tx, cu_tx)`,
  `cu_tx = 32`, instead of `64 >> ss`), fixed on that lane's UNMERGED branch.
  The 2- and 4-sample movements are the key frame's 49 chroma samples no longer
  poisoning the inter frames' prediction source — this fix's whole downstream
  contribution. Whoever merges that r1 gets the 6-frame stream.
* **The prior report's delta magnitudes are refuted** (§1). Its count, extents,
  per-cell partiality and first sample all reproduce; its `+1 … +63` /
  `−54 … +47` does not. Anyone matching on magnitude will not match this cell.
* **Not merge-ready in the conflict sense:** the fix edits `sub8_leaf_chroma444`
  and `decode_inter_block`'s neighbourhood is a different function, but a
  materialised merge has not been compiled.
* **Housekeeping observed, not touched:** the primary checkout carries an
  untracked `lanes/av1probes.report.md` that is not mine (it appeared during
  this lane). I did not add, move or delete anything there; the primary's
  tracked files are unmodified.

## 8. Reproduce it

```bash
D=$HOME/.cache/loss444kf; mkdir -p $D
ffmpeg -v error -f lavfi -i "testsrc2=size=256x256:rate=25" -frames:v 6 \
       -pix_fmt yuv444p -strict -1 -f yuv4mpegpipe -y $D/6.y4m
~/.cache/aom-oracle/build/aomenc --lossless=1 --enable-palette=0 --enable-intrabc=0 \
       --codec=av1 --passes=1 --end-usage=q --threads=1 --row-mt=0 \
       --lag-in-frames=0 --kf-max-dist=100 --limit=6 --obu -o - - \
       < $D/6.y4m > $D/vc.obu      # 109215 B, sha256 4563a01f17786000…
python3 $D/split.py $D/vc.obu $D/kf.obu     # SEQ+TD+FRAME, 18140 B
EC_AV1_FINAL_DUMP=$D/orc_final ~/.cache/aom-oracle/build/aomdec --codec=av1 \
  --limit=1 --noblit -o /dev/null $D/kf.obu
EC_PRED=1 EC_AV1_PREFILT_DUMP=$D/our_pre EC_AV1_POSTDEBLOCK_DUMP=$D/orc_deblock \
  EC_AV1_POSTCDEF_DUMP=$D/orc_cdef \
  cargo run -p ec-av1 --example dump_yuv -- $D/kf.obu $D/ours   # or the gate:
EC_AV1_REQUIRE_AOMENC=1 CARGO_TARGET_DIR=$HOME/.cache/cargo-target-av1loss444kf \
  cargo test -p ec-av1 --lib a_lossless_444_sub8_rect_leaf_chroma_reach -- --nocapture
```

Analysis scripts (read-only, outside the repo): `split.py` (the key-frame OBU
split), `an.py` (the per-plane fingerprint), `pair.py` (the EC_PRED /
OUR_PRED ladder paired by coordinates), `sim2.py` (the libaom z3 + edge filter +
upsample transcription that closes prediction-vs-residual).
