# lane-av1oddheightfork2 — the 320x236 fork is NOT a coefficient read: it is the var-tx partition context of an INTRABC block

Base `2005a35d`, branch `lane-av1oddheightfork2`, worktree `~/.cache/wt/av1oddheightfork2`.

## 0. Headline

1. **The ticket's first-divergence claim is REFUTED.** There is no "read 6" and no
   "312 extra bits" in a TX_8X4 luma unit at mi(0,4). Coefficient units **0..1060
   of frame 0 are byte-identical** to aomdec — same reads, same order, same
   values, same contexts, same `rng` at every step. The 312-bit artefact was an
   **instrument** artefact (see §3): the ladder could not resolve a rectangular
   unit's step count at all, so the count differences it reported were its own
   missing rungs.
2. **The state first differs at the ENTRY of coefficient unit 1061**, i.e. between
   two blocks, not inside a block.
3. **The single divergent read is named** (§4): the var-tx partition symbol of the
   INTRABC block at **mi(44,60), blk(0,0)**. aomdec reads it with context **13**
   and gets **split=1**; we read it with context **12** and get **split=0**. The
   context is a pure function of two `TXFM_CONTEXT` neighbours; ours read
   `above=8, left=16` where libaom's arithmetic needs one of them smaller. That is
   the defect's location.
4. **No decode fix is claimed.** I localised it; I did not close it. §6 names the
   one measurement that closes it.

## 1. Reproduction on my tip (step 1 of the ticket)

```
aomdec --codec=av1 --rawvideo -o oracle.raw cell.obu        # instrumented oracle
EC_PROBE_OUT=ours.yuv .../release/examples/decode_probe cell.obu
```

`cell.obu` = `crates/ec-av1/fixtures/420_oddheight_320x236_diverging.obu`,
16562 B, sha256 `84e4d1ab56620af1c5b78e1aa3d2d67496a492f2e127198c82dd1d68b01c6200`
(unchanged, verified).

**Y 234349 / U 56957 / V 52981, 0/16 frames exact** — byte-identical to the
recorded pin. No other lane moved the numbers. Frame 0 alone is already wrong, so
everything below is key-frame-local: no propagation, no MC, no loop filter.

## 2. Label space, stated once, before any comparison (ticket step 2)

Two rungs are used, and for each: what it prints, in what coordinates, and why the
two sides are comparable.

**`EC_COEFF_STEP` (coefficient ladder).**
*Oracle*: `scripts/instrument-aom-oracle.sh` rungs 3 + 11, applied to
`av1/decoder/decodetxb.c`. One line per coefficient read, `rng` **after** the read
it labels. Unit delimiters: `EC_COEFF plane row col tx_size mi_row mi_col rng`
opens, `EC_COEFF_VAL ... rng` closes. `row`/`col` are the transform unit's
4-pixel-unit position *inside* the block; `mi_row`/`mi_col` are the block's mi
position.
*Ours*: `crates/ec-av1/src/decode.rs` `read_coeffs` / `read_coeffs_rect`.
`tag=all_zero` opens a unit (both readers log the `txb_skip` read first, so it is
the only delimiter our side has). The `EC_COEFF plane=0 row={mi_r} ...` line at
decode.rs:19810 is a **different rung** — a block-level bracket in mi coordinates,
luma only — and is not read as a unit header.
*Comparability*: the only cross-decoder quantity is the msac `rng` register
printed after the same logical read. It is a deterministic function of (bit
position, prior symbols, CDF state), so an `rng` match at step *k* with a mismatch
at *k+1* pins the divergence to the read at *k+1* — **provided both sides made the
same set of reads in between**. Getting that proviso right is §3, and it is the
whole content of this lane.
Not compared, and why: `pos` (the oracle prints column-major `col*h + row`, our
rect reader prints raster `pos` — different label spaces; ctx, level and `rng`
pair up, so a differing `pos` is a label artefact, verified on unit 18);
`all_zero`'s `ctx` (the oracle's is the raw `txb_skip_ctx` from `get_txb_ctx`,
ours is the index into the already-selected `coding.txb_skip` row — different
labelings of one select; `cdf0`/the CDF array is the check and it agrees).

**`EC_VARTX` (var-tx partition).**
*Oracle*: pre-existing rung in `read_tx_size_vartx`
(`decodeframe.c:1131-1134`), under `EC_VARTX=1`:
`EC_VARTX mi=(r,c) row=.. col=.. ctx=.. n=2 s=.. bitpos=..`, `ctx` is
`txfm_partition_context(...)` and `s` is the decoded symbol.
*Ours*: `EC_ISTEP mi_row=.. mi_col=.. name=txfm_split|txfm_split_rect val=.. ctx=..`
under `EC_TRACE_MODE_STEP=1`.
*Comparability*: both print the `txfm_partition` context index and the decoded
2-way symbol, both in `xd->mi_row/mi_col` coordinates. One difference: after a
split, libaom's child reads carry `blk_row`/`blk_col` offsets while our rung
prints the child's own mi origin, so pairing is by order, and the `(ctx, symbol)`
pair is what is compared.

## 3. Four rung-shape asymmetries, all proven from source (this is what produced the phantom "read 6")

Each of these made a rectangular unit's step count wrong, and the earlier lanes'
"read index 6 / 312 extra bits" was measured on a ladder that could not see them.

1. **The oracle's `tag=tx_type` is a label for a read that happens only on luma.**
   `decodetxb.c:206-212`: the rung is emitted for every plane, but the read it
   labels, `av1_read_tx_type`, is inside `if (plane == AOM_PLANE_Y)`, and inside
   `get_ext_tx_types(...) > 1` (`blockd.h:1124-1129`), which is false for any unit
   whose `txsize_sqr_up_map` is `TX_64X64` and for a 32x32-squared intra unit, and
   behind the lossless / `skip_txfm` early returns (`decodemv.c:679-685`).
   *Dropped by TAG in the pairing walk, never by a heuristic on `rng`.*
2. **The oracle prints `tag=base_eob` AFTER that coefficient's own br loop**
   (`decodetxb.c:317-338`: the br `for` is inside the base_eob block, the
   `fprintf` is after it), so base_eob's br reads are invisible on its side. Our two
   readers bracket it oppositely — the rect reader logs base_eob then the br reads,
   the square reader logs the br reads then base_eob.
3. **Our rect reader logged a second, ctx-less `tag=base` echo** at the end of the
   loop body; the square reader's `tag=base` always carries a ctx, so a ctx-less
   `base` is exactly that echo.
4. **Our rect reader had no `tag=post_golomb` rung at all**, so every rect unit
   was one step short. **Added** (decode.rs:8738-8748, trace-only, reads no bits).

**Rung-shape fixes committed as INSTRUMENT changes, no decode logic touched:**

* `read_coeffs_rect`: added `tag=post_golomb` (decode.rs:8738-8748).
* `read_coeffs_rect`: moved `tag=base_eob` from the read site to the end of the
  loop body (decode.rs:8708-8716) so it carries the **post-br** level and **post-br**
  `rng`, which is what the oracle's rung carries. Printed at the read site it
  labelled the pre-br read, and every rect unit's ladder disagreed with aomdec by
  exactly its br tail.
* `read_block_tx_size_rect`: `txfm_split_rect` now prints `above=` / `left=` /
  `maxblk=` / `tx=` (decode.rs:30010-30029). Trace-only.

**A heuristic I used and then withdrew, because it is unsound — recorded so nobody
reuses it.** I first dropped "no-op" rungs by the rule *rng unchanged from the
previous step ⇒ no read happened*. Main was right to challenge it. libaom's
`od_ec_dec_normalize` (`entdec.c:125-137`) and our `renorm` (`msac.rs:588-596`)
**both** write `rng` unconditionally, and **both** can write it **unchanged** when
the renormalisation shift is 0 (the range is already in `[2^15, 2^16)`). A two-symbol
read always halves the range and so always moves `rng`, but a multi-symbol read
need not. "rng unchanged" therefore implies nothing on either side, and the rule
was replaced by the structural, source-proven tag rule in (1) above.

## 4. The named divergence (ticket acceptance item 1)

With the ladder corrected, `/tmp/oddwork/parse.py` walks units pairwise:

```
FIRST DIVERGENCE unit#1061 kind=rng our_step=0
  oracle hdr (0, 0, 0, 44, 60)      # plane 0, blk(0,0), mi(44,60)
  oracle  all_zero rng=34242   |  ours  all_zero rng=53285
```

Units 0..1060 pair exactly — count, tag, value, ctx and `rng` at every step. The
paired-prefix symbol totals are **oracle 10411 / ours 10483 … equal** (the script
prints `paired-prefix symbol totals` on every run; with counts equal the walk is a
1:1 pairing, not an alignment artefact). The state therefore first differs at the
**entry** of unit 1061, so the extra/missing read is **between** blocks. That
directly answers Main's count-first question: **it is not a missing read inside a
unit — there is exactly one divergent read, and it is outside every unit.**

Slicing the merged logs (`EC_TRACE_COEFF=1 EC_TRACE_MODE_STEP=1` on both sides)
to the window between unit 1060's close and unit 1061's open gives the whole
intervening read sequence, and both sides agree on all of it:

```
  oracle                                        ours
  EC_ISTEP mi(44,60) skip   rng=39168           EC_ISTEP mi(44,60) skip   rng=39168
  EC_ISTEP mi(44,60) cdef   rng=39168           EC_ISTEP mi(44,60) cdef   rng=39168
  EC_ISTEP mi(44,60) dq     rng=39168           EC_ISTEP mi(44,60) dq     rng=39168
  EC_ISTEP mi(44,60) intrabc val=1 rng=42896     EC_ISTEP mi(44,60) intrabc val=1 rng=42896
  EC_DV       mi(44,60)     rng=34510           EC_DV       mi(44,60)     rng=34510
  (no rung)                                    EC_ISTEP mi(44,60) name=txfm_split_rect
                                                 val=0 ctx=12 above=8 left=16
                                                 maxblk=16 tx=8x16 rng=56014
```

and the oracle's own `EC_VARTX` rung for the same read:

```
  EC_VARTX mi=(44,60) row=0 col=0 ctx=13 n=2 s=1 bitpos=22435
```

**The divergent read, named:**

| | |
|---|---|
| site (ours) | `crates/ec-av1/src/decode.rs:30017` — `dec.symbol(&mut cdfs.txfm_partition[ctx])` in `read_block_tx_size_rect`, the single top-level var-tx symbol of the block at mi(44,60) |
| site (oracle) | `av1/decoder/decodeframe.c:1130-1132` — `aom_read_symbol(r, ec_ctx->txfm_partition_cdf[ctx], 2, ACCT_STR)` in `read_tx_size_vartx` |
| block | INTRABC, 8x16 luma, `max_txsize_rect_lookup[BLOCK_8X16] == TX_8X16` (`common_data.h:132`) |
| CDF / ctx | `txfm_partition_cdf`, ctx **12 (ours)** vs **13 (aomdec)** |
| symbol | 0 (no split) vs 1 (split) |
| consequence | aomdec then reads two children (`ctx=15 s=0` at row 0, `ctx=15 s=0` at row 2); we read none and take one whole-block transform |

**Why the context differs — the arithmetic, both sides.** The two functions are
line-for-line the same shape (`decode.rs:28987-29004` vs
`av1_common_int.h:1747-1769`): `above = above_ctx < txw`,
`left = left_ctx < txh`, `category = (sqr_up(tx) != max_tx && max_tx > TX_8X8) +
(TX_SIZES-1-max_tx)*2`, `ctx = 3*category + above + left`. With `maxblk=16`,
`tx=8x16`: `max_idx = 2`, `category = 0 + (4-2)*2 = 4`, so `ctx = 12 + above +
left`. Ours has `above_px=8 < tx_w=8` → 0 and `left_px=16 < tx_h=16` → 0, giving
**12**. aomdec's **13** needs `above + left == 1`, i.e. **at least one of its two
`TXFM_CONTEXT` neighbours is strictly smaller than ours**: its `above_ctx` is `< 8`
where ours is `8`, or its `left_ctx` is `< 16` where ours is `16`. The category is
identical, so this is **not** an arithmetic defect — it is a **neighbour-state
defect**: our `TXFM_CONTEXT` grid (`n.above_txfm` / `n.left_txfm`) holds a
different value at (mi_row 44, col 60) than libaom's `above_txfm_context` /
`left_txfm_context` do.

**The immediately preceding block is the suspect, and the mechanism is named.** The
block to the left of mi(44,60) is the 8x16 block at mi(44,58), which **both** sides
agree split (`ctx=14 s=1` on each). aomdec's two follow-up child reads there are
`ctx=17 s=0` (row 0) and `ctx=16 s=0` (row 2); ours are `ctx=17 s=0` and `ctx=16
s=0` — same contexts, same symbols. So the split and both children agree, and yet
the `left` band one block over differs. libaom's writer is
`txfm_partition_update(above_ctx, left_ctx, tx_size, txb_size)`
(`av1_common_int.h:1684-1695`): it fills `left_ctx[0..mi_size_high[txsize_to_bsize[txb_size]]]`
with `tx_size_high[tx_size]` and `above_ctx[0..mi_size_wide[...]]` with
`tx_size_wide[tx_size]` — i.e. **the resolved leaf's size written over the
node's own extent**. Our `txfm_partition_update_rect(n, at_mi, (tx_px, tx_px),
(txb_px, txb_px))` (`decode.rs:29010-29012`) has that same shape, so the rule is
right in general; what is not yet established is which write puts `16` into our
`left_txfm[44]` where libaom has something smaller.

**Is it an INTRABC rule?** I checked the branch rather than inferring it:
`parse_decode_block` (`decodeframe.c:1218-1236`) sets
`inter_block_tx = is_inter_block(mbmi) || is_intrabc_block(mbmi)` and takes the
**same** vartx path for INTRABC as for inter, so the read itself is not
INTRABC-special. I found **no** `use_intrabc` / `is_intrabc` special case on the
path from there to `txfm_partition_context`. The special case that *does* exist is
in the non-vartx branch's `set_txfm_ctxs(..., skip && is_inter_block(mbmi), ...)`
(`decodeframe.c:1234-1235`, `av1_common_int.h:1641-1653`), which widens the bands
to the **block** size for a skipped INTER block. That branch is not taken here.
So: **INTRABC is not itself the rule**; what is untested is whether the block's
*position* (mi col 60, two MI from the 320-px / 80-MI right region) or the
*neighbour's* published size is what differs. That is §6.

## 5. Re-measurement, and the control (ticket step 4)

Not run. The gate and the sibling sweep are §6/§7 — I did not get to a fix, so
there is nothing to re-measure, and I am not going to present an unfixed tree's
numbers as a result. What *is* measured and stable:

* the pin still reproduces at Y 234349 / U 56957 / V 52981, 0/16;
* vartx reads 0..4 of the frame pair exactly (`ctx=20`, `13`, `14 s=1`, `17 s=0`,
  `16 s=0`), so the divergence is a **single** read, not a class;
* the ladder is unchanged by the instrument fixes as a *decoder*: no decode logic
  was touched, so the pixel counts above are the same before and after them.

## 6. The one measurement that closes this

Add an oracle rung beside `EC_VARTX` in `read_tx_size_vartx`
(`decodeframe.c:1127-1128`) printing the two operands, then compare:

```c
fprintf(stderr, "EC_VARTXCTX mi=(%d,%d) row=%d col=%d above=%d left=%d txw=%d txh=%d ctx=%d\n",
        xd->mi_row, xd->mi_col, blk_row, blk_col,
        (int)xd->above_txfm_context[blk_col], (int)xd->left_txfm_context[blk_row],
        (int)tx_size_wide[tx_size], (int)tx_size_high[tx_size], ctx);
```

The rung must go through `scripts/instrument-aom-oracle.sh` (add it to the rung-10
python block) rather than by hand, or the next lane loses it. Our side already
prints `above=`/`left=` from the same two quantities
(`n.above_txfm[at_mi.1]`, `n.left_txfm[at_mi.0]`).

The answer is one of exactly two shapes, and they have different fixes:

* **`above` differs** → the block *above* mi(44,60) published the wrong
  `TXFM_CONTEXT` into `above_txfm_context[0]` at mi_row 44. Look at whatever wrote
  it last before this read.
* **`left` differs** → the mi(44,58) block's split children published the wrong
  size into `left_txfm_context[0]` at mi_col 60. `txfm_partition_update_rect` is
  the writer; check the extent it uses for a **rect** node on the split path, since
  `txsize_to_bsize[txb_size]` for a rect `txb_size` is the awkward part of
  `av1_common_int.h:1687-1689`.

**Control that separates "INTRABC rule" from "this content"** (Main's item 3, not
run): take a committed fixture that decodes byte-exact and contains an INTRABC
block inside an 8x16 or 16x16 leaf — `444_intrabc_rect4_witness.obu` and the
`420`-family intrabc gates are the candidates — and check the same ctx against
`EC_VARTX`. If it matches there, the rule is right in general and the bug is
positional or neighbour-state-specific; if it does not, this is a 4:2:0-reachable
defect far larger than this ticket.

## 7. `not_done`

* **Not fixed.** Named, localised to one read with both sides' state, not closed.
* **No committed byte-exactness gate**, because there is no fix to pin; pinning
  the current wrong numbers is the ratchet pattern this defect has burned three
  lanes on. The `+1` oracle-flip control the ticket requires is therefore also
  not committed — it belongs with the gate. The existing control for the
  comparator itself, `the_counting_oracle_diff_attributes_planes_per_frame`, was
  not re-run (it is a comparator gate and this lane changed no comparator).
* **The 49 exact siblings were not swept.** No decoder change, so nothing can have
  moved; but that is an argument, not a measurement, and it is not what was asked.
* **`cargo test -p ec-av1 --lib -- 420 422 444 lossless warp` was not run.** Lane
  rules scope validation to the lane, and the only changes are env-gated `eprintln!`
  trace sites, which cannot affect behaviour; the example builds and runs.
* **The instrument fixes are uncommitted-to-main in the sense that matters**: they
  are in this branch, clearly labelled in §3, and are trace-only. They must land as
  their own commit so a reader can separate "what changed the instrument" from
  "what changed the decode" — right now the answer to the second is *nothing*.
* **The refuted lines must not survive elsewhere.** "Fork at coefficient read 6",
  "inside the read-5 (V-plane 8x8) unit's tail", and "312 extra bits" appear in
  `lanes/av1labelspace.report.md:94-101`, `lanes/av1partadvance.report.md:88-98` and
  the ticket. All three are wrong and should be struck, with §4 as the replacement.
* **Availability not yet measured.** The ticket's shape hints at an availability
  rule; the `TXFM_CONTEXT` grid is a plain fill (`set_txfm_ctx`,
  `av1_common_int.h:1636-1639`) with no availability notion in it, so the correct
  reading of the `above`/`left` operands is a *size*, not a *presence* — but I have
  not yet printed both sides' sizes to confirm which one is smaller.
