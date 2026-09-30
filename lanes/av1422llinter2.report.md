# lane-av1422llinter2 — the lossless 4:2:2 inter divergence is FIXED

> **CORRECTIONS from the independent refutation pass `lanes/refute-av1-w3a.report.md` (2026-09-30).** The
> fix, its class sweep and its reachability all CONFIRMED, and this merge's claim is the one the pass
> states most strongly: the three fixed arms were re-measured with temporary counters — single-ref /
> compound / intra-in-inter fire 131/356/202 on `W_intrabc`, 148/376/178 on `X_intrabc_tiled`,
> 3/166/565 on `Y_intrabc_10b`. Two arithmetic slips in this report: **§6 says "24 of 31 exact before"
> where its own table gives 23**, and the class sweep describes `decode.rs:24618`
> (`else if chroma_444 && lossless`) as "already shape-generic" when it is statically UNREACHABLE
> (24544 subsumes it) — the conclusion "no fourth reachable site" still holds, but the reason phrased there
> is wrong.


Tip: `main` = `cc211d13` (Merge lane-av1422llinter). Worktree
`/home/tahinli/.cache/wt/av1422llinter2`, branch `lane-av1422llinter2`, commit
`475c45ac`. One source file changed: `crates/ec-av1/src/decode.rs`. The 4:2:2
sequence-header bypass was applied, measured with, and reverted;
`git status` is clean on the lane tree and on the primary checkout.

**Result: `W_intrabc`, `X_intrabc_tiled` and `Y_intrabc_10b` go from
1-of-17 / 1-of-17 / 1-of-4 exact decode frames to 17-of-17, 17-of-17 and
17-of-17, on all three planes.** `ll422_noibc` goes 2/16 → 16/16. The
4:2:0 and 4:4:4 lossless controls are unchanged. §7 states plainly that none
of this is gate-able.

---

## 1. Geometry first (the brief's numbers were stale, again)

`W_intrabc.obu` (130 320 B) and `X_intrabc_tiled.obu` (131 696 B) are
**320x240 4:2:2, 8-bit**: 76 800 Y + 38 400 U + 38 400 V = **153 600 B per
frame**, and the instrumented oracle emits 17 DECODE-order frames of exactly
that size. `Y_intrabc_10b.obu` (202 281 B) is **300x256 4:2:2, 10-bit stored
16-bit LE**: 76 800 + 38 400 + 38 400 samples = **307 200 B per frame**, 17
decode frames. All three are already committed as fixtures
(`crates/ec-av1/fixtures/{W_intrabc,X_intrabc_tiled,Y_intrabc_10b}.obu`,
sha256 `0aad0d6f…`, `e7c0c60a…`, `ce84d7cb…`, pinned by
`scripts/fixture-library.tsv:204-206`).

Decode order confirmed by re-measurement: 17 coded frames, 16 shown, 6 of
those `show_existing`. `EC_AV1_FINAL_DUMP` is DECODE order on both sides and
`EC_ECDUMP_IN`'s bit position resets to 22 at the 9600th TU — the start of
decode frame 1. **Decode frame 0 is byte-exact on all three cells on all
three planes; decode frame 1 is the first inter frame and the first bad one.**

## 2. The named expression

**libaom, `av1/decoder/decodeframe.c:1196-1197`**, in `read_tx_size`:

```c
static TX_SIZE read_tx_size(const MACROBLOCKD *const xd, TX_MODE tx_mode,
                            int is_inter, int allow_select_inter, aom_reader *r) {
  const BLOCK_SIZE bsize = xd->mi[0]->bsize;
  if (xd->lossless[xd->mi[0]->segment_id]) return TX_4X4;
```

TX_4X4 for **every plane** of a lossless block — luma *and* chroma. That
`mbmi->tx_size` is then walked per plane in
`av1/decoder/decodeframe.c:299-302` (`decode_reconstruct_tx`) and
`ec_read_coeffs_txb_impl` (`decodetxb.c:449-469`):

```c
  const BLOCK_SIZE plane_bsize =
      get_plane_block_size(bsize, pd->subsampling_x, pd->subsampling_y);
  TXB_CTX txb_ctx;
  get_txb_ctx(plane_bsize, tx_size, plane, pd->above_entropy_context + col,
              pd->left_entropy_context + row, &txb_ctx);
```

and the symbol the fork turned on is **`av1/decoder/decodetxb.c:158`**:

```c
  const int all_zero = aom_read_symbol(
      r, ec_ctx->txb_skip_cdf[txs_ctx][txb_ctx->txb_skip_ctx], 2, ACCT_STR);
```

`get_plane_block_size` (`av1/common/blockd.h:1186-1193`) is
`av1_ss_size_lookup[bsize][ss_x][ss_y]` — **the plane block is PER-AXIS**. For
`BLOCK_8X8` that is BLOCK_8X8 at 4:4:4, **BLOCK_4X8 at 4:2:2**, BLOCK_4X4 at
4:2:0. The chroma branch of `get_txb_ctx` (`av1/common/txb_common.h:429-435`,
the `get_txb_ctx_4x4` specialisation) is then

```c
      const int ctx_base = get_entropy_context(tx_size, a, l);
      const int ctx_offset = (num_pels_log2_lookup[plane_bsize] >
                              num_pels_log2_lookup[txsize_to_bsize[tx_size]])
                                 ? 10
                                 : 7;
      txb_ctx->txb_skip_ctx = ctx_base + ctx_offset;
```

`get_entropy_context` (`av1/common/entropy.h:87-93`, `return` at :170) is a
**boolean** combine for TX_4X4 — `above_ec = a[0] != 0; left_ec = l[0] != 0;`
then `combine_entropy_contexts` — so `ctx_base` is 0, 1 or 2.

**Ours, `crates/ec-av1/src/decode.rs:50012` before the fix** (and the same
line at `50970` and `51588`), the 8x8 inter leaf's lossless chroma arm:

```rust
if chroma_w == 8 && lossless(fctx) && !mono(fctx) {
    let (ug, vg) = leaf8_inter_chroma_lossless(...)?;   // (8, 8) / stride 8 hardcoded
} else {
    u_grid = if chroma_422 {
        read_inter_plane_rect(dec, cdfs, chroma_set, (chroma_w, chroma_h),
                              chroma_w, 1, chroma_around[1], mode_for_tx, u,
                              cpx, cpy, su, Some(luma_tx_type),
                              None /* luma_skip_ctx */, fctx)?.0
```

`chroma_w == 8` is `SIDE >> ss_x == 8`, i.e. **ss_x == 0: the gate tests for
4:4:4**. At 4:2:2 `chroma_w` is 4, the gate is false, and the leaf's 4x8
plane block is coded as **one TX_4X8 unit at ctx_offset 7** (our port's
offset-7 rows are `txb_skip_chroma_8` rows 0..3, so `skip_ctx = above + left`
with no `+3`). The `None` is the offset itself:
`read_inter_plane_rect`'s `skip_ctx` (decode.rs:30529) is
`above + left + luma_skip_ctx.unwrap_or(0)`, and `multi.then_some(3)` was the
only thing that ever supplied the offset-10 rows.

**Why it is a ~90% first-inter-frame divergence while frame 0 stays exact.**
It is a pure CDF-ROW error on one 2-symbol read, not a read-count error: the
first 4:2:2 chroma unit is read against `txb_skip_cdf[..][1]` where libaom
uses `txb_skip_cdf[..][11]`, and the two rows answer `1` and `0` for the same
coder state. `1` means `all_zero` — our side skips the whole plane block,
libaom descends into its coefficients — so from that symbol on the two decoders
read a different number of symbols and the coder is desynchronised for the
rest of the frame. Frame 0 is intra/intrabc and never reaches
`decode_inter_block8`, so it cannot see it. The 4:2:0 and 4:4:4 arms are
correct precisely because the 4:4:4 shape is the one the gate admits, and
`chroma422_rect` (the one-unit 4:2:2 read) only ever fired at 4:2:2.

**Direct measurement of the wrong context** (oracle `EC_DBGCTX`, a rung that
prints the base, the offset and the final index):

```
EC_DBGCTX mi=2,0 plane=1 tx=0 above0=7 left0=0 base=1 off=10 ctx=11 cellsA=7, cellsL=0,
```

`off=10` is libaom's own number for this unit. Ours, from
`EC_COEFF_STEP tag=all_zero` on the same unit:

```
EC_COEFF_STEP tag=all_zero plane=1 ctx=1 entry=33288 all_zero=1 rng=59556 bit=0 post_bit=1093
```

`ctx=1` where the oracle has `ctx=11`, from an **identical coder state**
(`entry=33288` is the oracle's `ECIN=(3851,33288,1076)`), and `all_zero=1`
where the oracle reads `s=0`.

## 3. How it was localised (the ladder, not a guess)

1. `EC_SYMR` on both sides (`msac.rs:489` / libaom `aom_dsp/bitreader.h:262`)
   gives a read-by-read stream. Two conventions are excluded from the
   comparison, each measured: `pre_bit` differs by a **constant 15** (ours
   counts from the end of the buffer), and `cdf0` is PRE-adapt on ours and
   POST-adapt on the oracle. `cdf=` (the table name) is one-shot on the oracle
   and unlabelled on ours. Comparing on `(pre_value, pre_range, n, symbol)`,
   the ladders agree for **55 113 reads** and first differ at index 55 113,
   at the oracle's `decodetxb.c:158`.
2. The oracle's `EC_ECDUMP_IN` record carrying that exact pre-state is
   `plane=1 mi=(2,0) bc=0 br=0 tx=0 ctx=11` — a chroma TX_4x4 unit of the
   8x8 inter block at `mi(2,0)`, `mode=13` (NEARESTMV), `rf=(1,0)`,
   `mv0=(0,0)`, `skip=0`, with the block's four luma TX_4x4 units immediately
   before it and all four matching ours.
3. A `#[track_caller]` `#[inline(never)]` breadcrumb in
   `read_inter_plane_rect` (temporary, reverted) pinned the ours-side unit:
   `w=4 h=8 plane=1 off=None lossless=true`, 418 of them over the cell —
   every one a wrong single-TX_4X8 read at offset 7.

**The oracle's unit walk, read off `EC_ECDUMP_IN` for decode frame 1** (this
is the shape the fix has to reproduce): every TU is `tx=0` (TX_4x4); per
block the luma units come first, then **all** of plane 1, then **all** of
plane 2; inside a plane the order is **row-major** (`br` outer, `bc` inner).
An 8x8 leaf codes `plane=1 bc=0 br=0` then `bc=0 br=1`; a 16x16 leaf codes
`plane=1` as `(0,0) (1,0) (0,1) (1,1) (0,2) (1,2) (0,3) (1,3)`. That is
exactly what `read_inter_chroma_lossless` already walks, which is why the fix
routes to it instead of writing a new walk.

## 4. The fix

`475c45ac`, one file. The intra-frame 8x8 leaf already had the right predicate
— `(chroma_444 || chroma_422) && lossless(fctx)` at `decode.rs:24544`, with a
`CHROMA422_LOSSLESS_4X4_HITS` counter and `ll422_allintra` byte-exact. The
three inter / intra-in-inter 8x8-leaf arms had the wrong one.

1. `leaf8_inter_chroma_lossless` now takes the leaf's **per-axis** plane block
   `(blk_w, blk_h)` and the composed grid's `stride`, instead of hardcoding
   `(8, 8)`/`8`; its grids are `stride * blk_h`. At 4:4:4 that is the
   previous `(8, 8)`/`8`/`64` verbatim.
2. All three gates became `lossless(fctx) && (chroma_w > 4 || chroma_h > 4) && !mono(fctx)`
   — the same predicate `read_inter_chroma_lossless` already uses to choose the
   offset-10 rows. 4:2:0's 4x4 plane block is the one shape that keeps the
   single-unit read, and it keeps it.
3. The intra-in-inter arm additionally passed `(chroma_w, chroma_w)` where the
   plane block is `(chroma_w, chroma_h)`, and `Grid::Zero(64)` where it is
   `chroma_w * chroma_h`.

**Class sweep (same batch, same commit).** Every lossless chroma gate in
`decode.rs` was enumerated. The intra-frame leaf (24544, 24618, 24752) and the
`>=8x16` inter arms (42244, 44025) and the sub-8 rect inter arm (18368) were
already shape-generic. The three 8x8-leaf sites were the only ones keyed to a
chroma format's shape, and all three are fixed; no fourth exists.

**Non-vacuity of the new arm, measured** (gate counters, `W_intrabc`, bypass
on, before → after):

| counter | before | after |
|---|---|---|
| `chroma422_rect` (the one-unit 4:2:2 read) | **88** | **0** |
| `chroma422_sub8` (the per-4x4-unit 4:2:2 walk) | 169 | **391** |
| `chroma422_square` (the 64x64 lossless walk) | 3312 | 3312 |

The 88 single-unit reads are gone and 222 unit reads moved to the 4x4 walk.
(`intra128_lossless` 8 → 0 and `rect4_16_pair.lossless_chroma` 106 → 72 also
move, because the **before** counts are read off a desynchronised parse that
wanders into shapes the bitstream never codes; the after counts are the truth
and the output is byte-exact.)

## 5. Per-cell, per-plane, per-decode-frame before → after

Comparator: `/home/tahinli/.cache/lane2/beforeafter.py` and
`cmp422.py` / `cmp422_10.py`, against the instrumented oracle
`/home/tahinli/.cache/aom-oracle/build/aomdec` (`EC_AV1_FINAL_DUMP`, DECODE
order). BEFORE = a binary built at `cc211d13`, AFTER = this tip, **both with
the same one-line header bypass**.

### W_intrabc.obu — 320x240 4:2:2 8-bit (Y 76 800, U 38 400, V 38 400)

| decode | Y before | Y after | U before | U after | V before | V after |
|---|---|---|---|---|---|---|
| f0 | 0 | **0** | 0 | **0** | 0 | **0** |
| f1 | 68878 | **0** | 33705 | **0** | 36125 | **0** |
| f2 | 61759 | **0** | 33722 | **0** | 31699 | **0** |
| f3 | 38655 | **0** | 22290 | **0** | 22672 | **0** |
| f4 | 60575 | **0** | 30740 | **0** | 31932 | **0** |
| f5 | 62519 | **0** | 32023 | **0** | 32766 | **0** |
| f6 | 47888 | **0** | 25253 | **0** | 26030 | **0** |
| f7 | 55451 | **0** | 29072 | **0** | 29251 | **0** |
| f8 | 64691 | **0** | 33976 | **0** | 34825 | **0** |
| f9 | 68648 | **0** | 32663 | **0** | 34986 | **0** |
| f10 | 63161 | **0** | 34011 | **0** | 34183 | **0** |
| f11 | 67701 | **0** | 33187 | **0** | 34221 | **0** |
| f12 | 67757 | **0** | 32706 | **0** | 35316 | **0** |
| f13 | 61672 | **0** | 35592 | **0** | 35639 | **0** |
| f14 | 70701 | **0** | 34980 | **0** | 36703 | **0** |
| f15 | 65392 | **0** | 33087 | **0** | 34597 | **0** |
| f16 | 61404 | **0** | 31116 | **0** | 32791 | **0** |

First differing sample before, decode f1: `Y(r0,c32)` oracle 81 / ours 83,
`U(r0,c8)` 88 / 86, `V(r0,c13)` 247 / 246. **`W_intrabc`: 1/17 → 17/17 exact.**

### X_intrabc_tiled.obu — 320x240 4:2:2 8-bit

| decode | Y before | Y after | U before | U after | V before | V after |
|---|---|---|---|---|---|---|
| f0 | 0 | **0** | 0 | **0** | 0 | **0** |
| f1 | 54402 | **0** | 27737 | **0** | 26919 | **0** |
| f2 | 52502 | **0** | 26535 | **0** | 24645 | **0** |
| f3 | 59058 | **0** | 28274 | **0** | 27378 | **0** |
| f4 | 42325 | **0** | 19863 | **0** | 20461 | **0** |
| f5 | 61220 | **0** | 25404 | **0** | 25952 | **0** |
| f6 | 61042 | **0** | 29397 | **0** | 29989 | **0** |
| f7 | 51250 | **0** | 24985 | **0** | 25541 | **0** |
| f8 | 62799 | **0** | 31017 | **0** | 31519 | **0** |
| f9 | 59296 | **0** | 29642 | **0** | 28010 | **0** |
| f10 | 57825 | **0** | 28145 | **0** | 27357 | **0** |
| f11 | 64944 | **0** | 31939 | **0** | 31561 | **0** |
| f12 | 60101 | **0** | 30189 | **0** | 29064 | **0** |
| f13 | 59572 | **0** | 29504 | **0** | 28687 | **0** |
| f14 | 54986 | **0** | 27697 | **0** | 27353 | **0** |
| f15 | 62070 | **0** | 30998 | **0** | 30081 | **0** |
| f16 | 55335 | **0** | 28123 | **0** | 27374 | **0** |

**`X_intrabc_tiled`: 1/17 → 17/17 exact.**

### Y_intrabc_10b.obu — 300x256 4:2:2 10-bit (differing SAMPLES)

| decode | frames emitted before / oracle | Y before | Y after | U before | U after | V before | V after |
|---|---|---|---|---|---|---|---|
| f0 | 4 / 17 | 0 | **0** | 0 | **0** | 0 | **0** |
| f1 | | 63609 | **0** | 33415 | **0** | 31993 | **0** |
| f2 | | 53597 | **0** | 29657 | **0** | 29215 | **0** |
| f3 | | 61289 | **0** | 33685 | **0** | 33657 | **0** |
| f4..f16 | 0 / 13 | — | **0** | — | **0** | — | **0** |

**The before build emitted only 4 of the oracle's 17 decode frames and then
stopped silently.** The after build emits 17 and **all 17 are byte-exact on
all three planes** (76 800 + 38 400 + 38 400 samples each, 307 200 B). This
settles the two items lane-av1422llinter flagged as unverified:

* the `Golomb tail longer than this decoder reads` refusal in the debt **does
  not exist** — no message is printed, before or after; and
* the "4 of 17 frames then silence" was **this desync**, not a separate
  defect. `Y_intrabc_10b` needs no handover.

### Oracle-flip control (both cells, both comparators)

| control | result |
|---|---|
| W: XOR `0x01` into oracle decode f0 sample 0 | exactly **1** differing byte, reported at f0 plane Y (0,0); f1 stays exact |
| X: XOR `0x01` into oracle decode f1 sample 0 | exactly **1** differing byte, reported at f1 plane Y (0,0); f0 and f2 stay exact |

The comparator bites on a single flipped byte, so the 17/17 rows are not a
comparator that has stopped comparing.

## 6. The rest of the 4:2:2 corpus, and the 4:2:0 / 4:4:4 controls

`/home/tahinli/.cache/lane2/census422.sh` — every `.obu` under
`av1422lpf/` and `av1422lpf/lossy_all/`, before and after, same oracle.
"bad-frames" counts decode frames that differ from the oracle in any byte.

| cell | oracle frames | bad before | bad after |
|---|---|---|---|
| **W_intrabc** | 17 | 16 | **0** |
| **X_intrabc_tiled** | 17 | 16 | **0** |
| **Y_intrabc_10b** | 17 | 16 (only 4 emitted) | **0** (17 emitted) |
| **ll422_noibc** (probe) | 16 | 14 | **0** |
| 422_allskip_2f | 2 | 0 | 0 |
| 422_intrabc_sb128_strip | 5 | 0 | 0 |
| 422_intrabc_sb128_strip_notxsearch | 5 | 0 | 0 |
| 422_residual_compound_warp_16f | 16 | 0 | 0 |
| 422_residual_compound_warp_nolr_16f | 16 | 0 | 0 |
| 422_sb128_3f | 3 | 0 | 0 |
| ll422_allintra (probe) | 1 | 0 | 0 |
| AA_inter_compound | 43 | 0 | 0 |
| AD_inter_nogm | 43 | 0 | 0 |
| A_testsrc2_cpu0 | 17 | 0 | 0 |
| B_testsrc2_cpu6 | 17 | 0 | 0 |
| C_mandel320 | 16 | 0 | 0 |
| D_bars | 16 | 0 | 0 |
| E_noglobal | 17 | 0 | 0 |
| F_allintra | 16 | 0 | 0 |
| H_10bit_testsrc2 | 17 | 0 | 0 |
| I_10bit_mandel | 16 | 0 | 0 |
| J_10bit_lr0 | 17 | 0 | 0 |
| K_sct | 17 | 0 | 0 |
| L_tiled | 17 | 0 | 0 |
| R_odd322x240 | 17 | 0 | 0 |
| T_tilecols2 | 17 | 0 | 0 |
| U_tilerows1 | 17 | 0 | 0 |
| V_tile2x2_odd | 17 | 0 | 0 |
| **AB_inter_warp_odd** | 43 | 43 | 43 |
| **O_odd322x242** | 17 | 17 | 17 |
| **Q_odd320x242** | 17 | 17 | 17 |
| **S_odd326x242_10b** | 17 | 17 | 17 |

**24 of 31 unique 4:2:2 cells exact before → 27 of 31 after.** The four
remaining are the **odd-HEIGHT** cluster lane-av1422llinter flagged in its §4b
as a different mechanism: all 242 px tall, all failing at decode **frame 0**
(intra, no reference slot read), and `R_odd322x240` — odd *width*, even
height — is exact. Untouched here, as instructed.

**4:2:0 / 4:4:4 lossless controls** (`regress_420/`, `regress_444/`,
`probe/ll420_allintra`, `probe/ll444_allintra`), all before-and-after:

| cell | framesize | oracle | bad before | bad after |
|---|---|---|---|---|
| ll420_a / ll420_b / ll420_c / ll420_d | 115 200 | 16 | 0 | 0 |
| ll444_a / ll444_b | 230 400 | 16 | 0 | 0 |
| ll444_c | 230 400 | 18 | 0 | 0 |
| ll420_allintra | 115 200 | 1 | 0 | 0 |
| ll444_allintra | 230 400 | 1 | 0 | 0 |

No byte moved at 4:2:0 or 4:4:4, which is the shape of the claim: 4:2:0's plane
block is 4x4 (gate false before and after, one unit at offset 7) and 4:4:4's
is 8x8 (gate true before and after, `(8, 8)`/`8` passed through verbatim).

## 7. Gate-ability: **NO.** Stated plainly, with the measurement.

4:2:2 is refused at the **sequence header** — `stream.rs:1803`,
`if seq.subsampling_x != seq.subsampling_y { return Err(...) }` — which
returns before any tile, mode-info or coefficient code runs. Measured on this
tip, on the committed fixture, with the bypass reverted:

```
$ decode_probe crates/ec-av1/fixtures/W_intrabc.obu
REFUSED: unsupported: AV1 decode_stream (a chroma format of 4:2:2
(subsampling_x != subsampling_y): this decoder decodes 4:2:0 and 4:4:4;
4:2:2 is not ported, and 4:4:0 (0,1) is not a codable cell)
```

**Isolation control.** With the bypass applied and nothing else changed, the
six committed 4:2:2 tests fail with `called Result::unwrap_err() on an Ok
value: [Picture { width: 320, height: 240, y: [74, 74, …` — i.e. with the
guard patched out the decoder *decodes* the pinned witnesses. Those six tests
are `a_non_420_subsampled_sequence_header_is_refused_by_name`,
`the_pinned_422_bigblock_witnesses_…`,
`the_pinned_422_intrabc_sb128_strip_witnesses_…`,
`the_pinned_422_lossless_inter_witnesses_…`,
`the_pinned_422_lr_off_witness_…` and
`the_pinned_422_residual_compound_warp_witness_…`. Every one of them asserts
**the header refusal**, which fires before the fixed code is reachable.
Measured, both trees with the bypass reverted:
`cargo test -p ec-av1 --lib -- 422 refuse` gives
**`34 passed; 0 failed`** at the clean parent `cc211d13` and
**`34 passed; 0 failed`** at this tip — identical, so all six are vacuous
with respect to this commit.

**So: the fixture bytes are committed, pinned and byte-exact-verified, but no
committed test can execute the code this commit changes, because the only
committed entry point refuses 4:2:2 first. A byte-exactness gate would have
to lift that refusal, which is the user's product decision and explicitly out
of scope here.** The evidence offered instead is measurement plus the
single-variable controls in §6 (one source, one encoder, three chroma formats
→ 4:2:0 and 4:4:4 exact before and after, 4:2:2 exact only after) and the
oracle-flip controls in §5. **No source-scan substitute is offered**: the
committed `the_422_lossless_inter_chroma_walk_sites_stay_per_axis` proves
spelling and nothing about pixels, and this commit's guard is a *predicate*,
which no spelling scan could distinguish from the predicate it replaced.

The path to a real gate, for whoever lifts the refusal: the three fixtures are
already in `crates/ec-av1/fixtures/` and already in
`scripts/fixture-library.tsv`, and their oracle decode-order dumps are exactly
the `EC_AV1_FINAL_DUMP` files §5 tabulates. `W_intrabc` and `X_intrabc_tiled`
then become 17/17 byte-exactness gates immediately, and should be
mutation-proved (revert the guard predicate → 16/17).

## 8. Regression

On the reverted (bypass-free) tree at `475c45ac`:

```
$ cargo test -p ec-av1 --lib -- 420 422 444 lossless warp intra \
    --skip bitrate_target_lands_within_5_percent_over_48_frames
test result: ok. 181 passed; 0 failed; 2 ignored; 0 measured; 618 filtered out; finished in 251.38s
```

**181 passed, 0 failed, 2 ignored, 618 filtered out** — the same count
lane-av1422llinter measured as its baseline at `cc211d13`, so the six fixes
that landed before this one are untouched. (`cargo check -p ec-av1
--all-targets` is clean.)

## 9. What was refuted, and how

| claim | verdict | how |
|---|---|---|
| "frame 0's exactness exonerates the coefficient path" | **refuted** | it exonerates the *intra* path only; frame 1 is the first to reach `decode_inter_block8`'s chroma arm at all, and the fork is on its 24th transform unit |
| "the divergence is a coefficient read-COUNT error (an extra or missing symbol)" | **refuted** | the forked read has an identical coder state (`entry=33288` both sides), the same alphabet (n=2), and differs only in the value — a wrong CDF ROW, not a wrong number of reads. The read count only diverges *downstream* of the bad symbol |
| "the oracle's chroma `txb_skip_ctx` is a sum of the neighbour contexts" | **refuted** | `EC_DBGCTX` prints `above0=7 left0=0 base=1`; `get_entropy_context` is a boolean combine (`entropy.h:93`, `return combine_entropy_contexts(...)` at :170), so 7+0 masks to 1 |
| "it is OBMC / warp / compound / the reference slot / the interpolating filter" | **refuted** | the fork is a *token-read* divergence with no MC in the path at all: it happens while reading the first chroma unit's `txb_skip`, before any prediction is applied. The prefilter-vs-final identity that exonerates the filters is inherited from lane-av1422llinter §2 and is not re-litigated |
| "the intra and inter paths need different fixes" | **refuted** | the intra-frame 8x8 leaf already had `(chroma_444 || chroma_422) && lossless` (decode.rs:24544) and `ll422_allintra` was byte-exact; the inter and intra-in-inter arms were the three outliers and all three now share one predicate |
| "4:2:0 or 4:4:4 might be affected too" | **refuted** | 7 lossless 4:2:0/4:4:4 control cells measured before and after, 0 bad frames in both directions; the guard is false at 4:2:0 in both and the (8,8)/8 arguments pass through verbatim at 4:4:4 |
| "the 4 odd-HEIGHT cells are this defect" | **refuted** | they fail at decode frame 0 (intra, no reference read) at 242 px height while `R_odd322x240` is exact, and they are byte-identical before and after this commit |
| "the `Golomb tail` refusal in the debt is real" | **refuted** | no message is printed on either build; the before build's 4-of-17 silent stop is this desync and disappears with the fix (`Y_intrabc_10b` is now 17/17) |

## 10. Handover

1. **Nothing is broken and nothing is handed over.** Three committed 4:2:2
   fixtures that were 1/17, 1/17 and 1/4 exact are now 17/17, 17/17 and
   17/17 on all three planes, and `ll422_noibc` is 16/16.
2. **Gating needs the refusal lifted** (§7); that is the user's call. When it
   is, `W_intrabc.obu` and `X_intrabc_tiled.obu` are the gates and the
   mutation is one predicate.
3. **The odd-HEIGHT cluster is untouched and unclaimed**: `AB_inter_warp_odd`,
   `O_odd322x242`, `Q_odd320x242`, `S_odd326x242_10b`, all 242 px tall, all
   failing at decode frame 0. Adjacent to the closed `lane-av1oddheightfork3`.
4. The instruments live in `/home/tahinli/.cache/lane2/`:
   `cmp422.py`, `cmp422_10.py`, `beforeafter.py` (per-plane comparators with
   the flip control), `symrpair.py` (the read-by-read `EC_SYMR` pairer, with
   the two conventions it excludes and why), `census422.sh`, `reg420444.sh`,
   `probe.sh`.
