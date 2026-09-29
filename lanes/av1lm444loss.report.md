# lane-av1lm444loss — H1: 4:4:4 LOSSLESS byte-exactness

**Verdict.** Lead (a) **FIXED**: the pinned `ll444_128root_lossless.obu` is now
byte-exact on all six frames and its `EC_SYMR` ladder is bit-identical to the
oracle (235,940 reads, zero fork; it forked at read 71,322 before). The defect
was **not** in the coefficient reader — it was the transform-unit **walk**: a
lossless block's TX grid was never clipped at the frame edge, so the decoder
read 256 units per inter frame that libaom never codes, and the fork lands on a
PLANE change (`plane=0` luma where the oracle had moved to `plane=1` chroma).

Lead (b) is **blocked by a separate, pre-existing defect**, diagnosed to a
one-line cause in §5. Not this lane's class; not fixed here.

**Tree.** `lane-av1lm444loss` off `a21f3680`, worktree `~/.cache/wt/av1lm444loss`.
One commit, `7b9f46fa`. Not pushed (Main merges).

## 1. Lead (a): reproduction and the first fork

`fixtures/ll444_128root_lossless.obu` (63429 bytes, 128x96 yuv444p, 6 frames).
Decoded-order dump vs the oracle's `EC_AV1_FINAL_DUMP`, at the base tree:

| frame | wrong samples | first differing byte |
|---|---|---|
| 0 | 0 | — |
| 1 | 5596 | 16544 |
| 2 | 25693 | 0 |
| 3 | 28492 | 0 |
| 4 | 32289 | 0 |
| 5 | 33561 | 0 |

`EC_SYMR` on both sides, one line per `symbol()` call, aligned on
`(value, range, symbol, post_rng)`:

```
index 71321  both      mi=(0,0)  decodetxb.c:158 (txb_skip)  s=1  pre=(35131,51766) post=51766
index 71322  ORACLE    mi=(0,0)  decodetxb.c:158            s=1  pre=(35131,51110) post=49256  cdf0=31697
index 71322  OURS      mi=(0,0)  (luma walk)                s=1  pre=(35131,51110) post=50351  icdf0=377
```

Both sides read the same site and the same 2-class `txb_skip` CDF. The CDF row
content diverges (`32768 - 377 = 32391` vs the oracle's pre-adapt `32391` at
71321, `31697` at 71322), so this is the two sides being on **different units**,
not the same unit read two ways.

`EC_COEFF_STEP tag=all_zero` names them:

```
[3967] ORACLE plane=0 bc=15 br=23 ctx=1  (luma, block row 23 = y 92..95)
[3968] ORACLE plane=1 bc=0  br=16 ctx=10 (chroma — the walk moved planes)
[3968] OURS   side=4      ctx=1          (still luma)
```

That is the whole story in one line: the oracle finished the luma plane at
transform row 24 and moved to chroma; we were still reading luma.

## 2. Mechanism: the missing frame-edge clip

libaom bounds a block's transform grid by the part of it **inside the frame**:

- `max_block_high` / `max_block_wide` (`av1_common_int.h:1579`, `:1565`) start
  from `block_size_high/wide[bsize]` and, when `mb_to_bottom_edge` /
  `mb_to_right_edge` is negative, add `>> (3 + ss)`, then `>> MI_SIZE_LOG2`.
- `mb_to_bottom_edge` is `GET_MV_SUBPEL((mi_rows - bh - mi_row) * MI_SIZE)`
  (`set_mi_row_col`, `av1_common_int.h:1360`), so the clip reduces exactly to
  the block's extent within the frame, in transform units.
- `decode_token_recon_block` then caps every mu chunk with
  `unit_height = ROUND_POWER_OF_TWO(AOMMIN(mu_blocks_high + row,
  max_blocks_high), ss_y)` — the out-of-frame units are never read.

Our decoder already had this exact arithmetic. `read_block_tx_size`'s **var-tx**
branch computes it, twenty lines below the lossless branch:

```rust
let max_w_mi = side_mi.min(mi_cols.saturating_sub(at_mi.1));
let max_h_mi = side_mi.min(mi_rows.saturating_sub(at_mi.0));
```

and `read_var_tx_size` bails on it (`if blk_row >= max_h_mi || blk_col >=
max_w_mi { return; }`). The **lossless** branch returned a plain
`0..side/MI` square, and so did `read_block_tx_size_rect`'s lossless branch.

**The arithmetic, exactly.** Frame 128x96. `side = 128`, so `side / MI = 32`;
`mi_rows = 2 * ceil(96/8) = 24`. `max_blocks_high = 128 + (-32 >> 3) = 96`,
`>> 2 = 24`. Clipped rows 24, columns 32.

| | luma units / frame | chroma units / frame | total |
|---|---|---|---|
| oracle | 24 x 32 = **768** | 2 x 24 x 32 = **1536** | **2304** |
| ours, before | 32 x 32 = **1024** | 1536 | **2560** |

The 256-unit excess is exactly the 8 unclipped transform rows x 32 columns past
the frame bottom, per inter frame. Chroma was already clipped — the
`read_inter_chroma_lossless` loop carries its own
`if cpx + ox >= u.true_width || cpy + oy >= u.true_height { continue; }` — which
is why only the LUMA side of the walk diverged.

## 3. The fix

Both lossless leaf builders now clip. `decode.rs`, `read_block_tx_size`:

```rust
let side_mi = side / MI;
let max_w_mi = side_mi.min(mi_cols.saturating_sub(at_mi.1));
let max_h_mi = side_mi.min(mi_rows.saturating_sub(at_mi.0));
```

and `read_block_tx_size_rect` the same over `(bw, bh)`. Two hit counters ride
along: `lossless_edge_clipped()` (blocks whose grid the clip shortened) and
`lossless_edge_clip_units()` (units removed).

**The clip is inert when the block fits** — for any block whose `at_mi + side/MI`
is inside the frame, `min` returns `side_mi` and the leaf vector is
byte-identical. That is the structural reason 4:2:0 cannot move, and the gate's
control arm pins it.

### After

| measurement | before | after |
|---|---|---|
| frames byte-exact vs oracle aomdec (decode order) | 1 of 6 | **6 of 6** |
| worst frame wrong samples | 33561 | **0** |
| `EC_SYMR` reads, ours | 258035 | **235940** |
| `EC_SYMR` reads, oracle | 235945 | 235945 |
| first `EC_SYMR` fork | read 71322 | **none, all 235940 paired** |

## 4. Gate

`stream::tests::a_lossless_block_clips_its_transform_grid_at_the_frame_edge`,
same fixture as its sibling
`a_lossless_444_128_root_lossless_stream_reads_chunks_chunk_major` (96 is not a
multiple of the 128 superblock — that is the entire reason this stream is the
witness).

| # | assertion | value |
|---|---|---|
| 1 | `lossless_edge_clipped()` delta | **5** = one 128 root per inter frame. Reads 0 on a frame that fits, so a stream that merely decodes cannot satisfy it |
| 2 | `lossless_edge_clip_units()` delta | **1280** = 5 x 256 (8 rows x 32 cols removed per root) |
| 3 | every sample of all 6 frames vs oracle `aomdec --rawvideo`, decode order | exact |
| 4 | control arm (`ll444_minp64_128root_control`): both clip counters | **0 / 0** — the clip is a no-op where blocks fit |

### Mutation proof (run, not asserted)

Reverting only the two clip expressions to the unclipped grid, keeping the gate
and the counters:

```
=== MUT clip ===
test stream::tests::a_lossless_block_clips_its_transform_grid_at_the_frame_edge ... FAILED
panicked at crates/ec-av1/src/stream.rs:6924:
  the frame-edge clip fired on 0/5 of the 128 roots -- it must be 0 on a frame
  that fits, so this is what makes the gate non-vacuous
  left: 0   right: 5
test result: FAILED. 0 passed; 1 failed
SUITE EXIT: 101
```

Same reverted build, decoded-order dumps vs the oracle — assertion 3's failure
mode, captured:

```
f0: 0 wrong          f3: 28492 wrong, first at byte 0
f1: 5596 wrong, first at byte 16544    f4: 32289 wrong
f2: 25693 wrong, first at byte 0        f5: 33561 wrong
```

File restored from a saved copy (not `git checkout --`, per
`skill://stash-red-before-proof`), re-verified with `diff -q`, then:

```
=== LIVE ===
test ... a_lossless_block_clips_its_transform_grid_at_the_frame_edge ... ok
test result: ok. 1 passed; 0 failed
EXIT: 0
```

## 5. Lead (b): blocked by a different defect (not fixed here)

`testsrc2 512x128 yuv444p --lossless=1 --cpu-used=2 --lag-in-frames=0
--kf-max-dist=100 --limit=6` does **not** reach the entropy fork the ticket
describes. It dies first, on the base tree, before and after this fix:

```
thread 'main' panicked at crates/ec-av1/src/decode.rs:2787:
assertion `left == right` failed       left: (4, 8)   right: (4, 4)
   4: <ec_av1::decode::TxParams>::run
   5: ec_av1::decode::push_mc_rect_tx
   6: ec_av1::decode::read_inter_plane_rect
   7: ec_av1::decode::decode_intrabc_rect
   8: ec_av1::decode::decode_leaf_rect
   9: ec_av1::decode::decode_key_frame_tile_with_cdfs
```

(release builds wrap the assert and panic one frame later at
`decode.rs:2823`, `range end index 4 out of range for slice of length 0`).

**Cause, measured.** A temporary env-gated probe at `decode_intrabc_rect`'s
residual split:

```
EC_PROBE_IBC mi=(28,106) bw=8 bh=16 lossless=true leaves=Some(8)
```

The luma leaves are all TX_4X4 (8 of them, 2 x 4) — the lossless branch is
correct. The `(4, 8)` comes from the **chroma** call in the same arm:
`decode_intrabc_rect` derives the chroma footprint as
`let (cw, ch) = (bw / 2, bh / 2); let (cpx, cpy) = (px / 2, py / 2);` — a
hardcoded 4:2:0 halving. At 4:4:4 that block's chroma plane block is 8x16, not
4x8, so a **lossless** 4x8 chroma rect transform reaches `TxParams::run`, whose
lossless arm requires 4x4 (the Walsh-Hadamard path). Same class the
`av1tilerows` lane landed in the lossless 16x4/4x16 chroma-pair walk: a
hardcoded 4:2:0-shaped chroma reach at 4:4:4.

Confirming it is the whole blocker: the same stream re-encoded with
`--enable-intrabc=0` (118365 bytes) decodes on this tree **byte-exact on all six
frames** against the oracle aomdec.

**Not fixed in this lane.** It is a different defect class from H1, in
`decode_intrabc_rect`'s chroma geometry rather than the coefficient reader or
the TX-4X4 path this lane owns, and that function carries other lanes' gates
(`a_lossless_444_intrabc_rect_leaf_walks_per_4x4_units`,
`a_skipped_lossless_intrabc_rect_strip_zeroes_its_entropy_bands`). The shape
correction is `bw >> ss_x(fctx)` / `bh >> ss_y(fctx)` on `cw`/`ch`/`cpx`/`cpy`;
dispatching it to the intrabc-rect owner is cheaper than widening this lane.

Also measured, so nobody re-derives it: the cached `g_512x128_notile.obu`
under `~/.cache/h4/` is byte-exact on all 7 decode-order frames on this tree —
its recipe carries `--enable-rect-partitions=0`, so it never reaches the
intrabc rect route.

## 6. Invariants — all green on this tree

| group | result |
|---|---|
| new gate `a_lossless_block_clips_its_transform_grid_at_the_frame_edge` | 1 passed, 0 failed (mutation red, §4) |
| all `lossless` + `444` gates (`cargo test -- lossless 444`) | **24 passed / 0 failed** |
| 4:2:0 lossless + frame-edge gates (17 scoped: `frame_edge`, `partial`, `edge`, `odd`, `lossless`, non-444) | **17 passed / 0 failed** |
| sibling `a_lossless_444_128_root_lossless_stream_reads_chunks_chunk_major` | ok — its parity assertions 2/3 (4 walks, 1536 chroma units per inter frame) are unchanged and still exact |
| `cargo check -p ec-av1` | 0 warnings, 0 errors |
| source-scan guards | untouched — no `include_str!` anchor was edited; the only test hunks are in `stream.rs`, the only non-test hunks in the two leaf builders plus the two counters |

Full-suite validation is Main's; the runs above are the scoped groups this
lane's invariant names.

## 7. Refutation notes for the reviewer

- The reserved 4:2:0 group-tail chroma SKIP arm and `last_intrabc` tails are
  outside every hunk — the diff is the two lossless leaf builders, two counters,
  two accessors, one gate.
- 4:2:0 unreachability is not the argument here and is not claimed: the clip
  changes behaviour for **any** subsampling when a lossless block overhangs the
  frame. The 4:2:0 argument is the `min` no-op above, pinned by the gate's
  control arm and by the 17 scoped 4:2:0 gates.
- The clip reuses the var-tx branch's own variables and its own
  `mi_cols`/`mi_rows` definition, so it introduces no new notion of the frame
  grid. Its only approximation is that `mi_rows` is `2 * ceil(h / 8)` rather
  than libaom's floor `h >> 3`; the two agree on every multiple-of-8 height,
  which is the fixture's 96, and differ by at most one 4-px transform row
  otherwise — the same approximation the shipped var-tx branch already carries.
