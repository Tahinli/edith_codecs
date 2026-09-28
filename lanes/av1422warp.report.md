# lane-av1422warp — the second 4:2:2 coverage witness: stream pixel-exact 16/16, fixture pinned, refusal KEPT

Base `a7d22aec`, worktree `~/.cache/wt/av1422warp`, branch `lane-av1422warp`, no
push. Target dir `$HOME/.cache/cargo-target-av1422warp`.

## Verdict

The charter asked for a pixel-exact 4:2:2 stream with real residuals, compound
and warped motion above the vertical midpoint. **It is pixel-exact**: all 16
frames match `aomdec --rawvideo` byte for byte, and the entropy stream pairs
246735-for-246735 against the instrumented oracle with no divergence anywhere.
The fixture is **pinned** and a **coverage gate is committed**. The
sequence-header refusal **stays** — see the lift section, which sets out why
"exact" and "covered" are not the same bar.

| commit | what it settles | before | after |
|---|---|---|---|
| `af3285d5` | **the lift-blocker fix**: `read_lr` stepped the row axis by the COLUMN axis's term, and the superblock extent was 2x too large — the two cancelled exactly at 4:2:0, so 4:2:0/4:4:4 decoded byte-exact while carrying the bug | first divergence at symbol 107517; 16 frames non-exact | **246735/246735 paired, 16/16 pixel-exact** |
| `4579d209` | OBMC strides the 4:2:2 chroma prediction with the square `chroma_side` | 0 frames decoded (panic) | 16/16 decode |
| `a9611ea2` | chroma-above neighbour read one mi column left of libaom's | frame 0 PREFILT 56142 diffs | 53395 diffs; first divergent block byte-exact |
| `a4532a41` → superseded | the OBMC pair-merge snap was clamped on the INDEX rather than the reported offset (reviewer P2) | read one column right of libaom, then skipped `mi_col + 1` | snap unconditional, offset saturates |

## Pinned, and the gate

`crates/ec-av1/fixtures/422_residual_compound_warp_16f.obu` — 38845 B, sha256
`d78e2afb43ce311d3d82335a945c6f80f62db4537966d881c1349a68b3aecb95`, fnv1a64
`0x0e73a51e2cc0c424`. Gate
`the_pinned_422_residual_compound_warp_witness_is_present_and_refuses_by_name`
— pin plus refuse-by-name, the established 4:2:2 pattern, because with the
header refusal standing no committed test *can* decode a 4:2:2 stream. It is
mutation-proven both ways (flipped fixture byte panics `bytes drifted`; wrong
refusal string panics `must refuse by name`).

## The stream (source recipe; the fixture is now committed)

- pinned `a1.obu` 38845 B, sha256 `d78e2afb43ce311d3d82335a945c6f80f62db4537966d881c1349a68b3aecb95`
- source `src422.y4m` sha256 `4d35eaf65d1a5541b3177e1183644c163b3868d8f141bed0ce9fdf833280ba9f`
- 4:2:0 control `a420.obu` sha256 `1f43ef1ace6f7fa2f38c2deff765dd0bc529cbf7432788c736c7261350525e5b`

Measured against `aomdec --rawvideo`: 2359296 bytes, sha256
`4bfc2395e5ca6ea178f606e6b1ced773e26fc2d85bc3a6cdb63a402939aeb203` on both
sides.

Rotzoom mandelbrot is lane-av1gwarp12's lever (a similarity transform is exactly
the family the 4-parameter ROTZOOM model expresses); `lag-in-frames=25` plus
`auto-alt-ref` gives the multi-reference compound needs. Decode is
`EC_AV1_ALLOW_422_PROBE=1` through the local patch-run-restore bypass, compared
against `aomdec --rawvideo`; the bypass is in neither commit.

## Defect 1 — OBMC chroma prediction stride (`4579d209`)

Site: `ObmcPlan.chroma_side`, `obmc_plan`'s parameter, `obmc_run`'s destructure,
consumed at the four chroma blend sites and the two `EC_MCB` prediction dumps.

Since lane-av1-422blockhalf the 4:2:2 chroma PREDICTION buffer is
`(chroma_stride, chroma_buf_h)` — the plane block's own per-axis shape — but
`obmc_run` strided it with the enclosing square `chroma_side`, so row `r` was
read at `r * side`. Measured on `a1.obu`: the 32x16 luma strip at px=160, py=0 has
`side=32, write_w=32, write_h=16, chroma_stride=16, chroma_buf_h=16`, so `pred_u`
is 256 samples and the left-pass chroma blend wrote row 15 at index `15*32 = 480`
(needs 520): `index out of bounds: the len is 256 but the index is 256` in
`obmc_blend_h`. Before: zero frames decoded. After: `OK: 16 frames decoded,
256x288`. Every other consumer of `pred_u`/`pred_v` in that block already strides
with `chroma_stride`, and the two are equal at 4:2:0 and 4:4:4.

## Defect 2 — the chroma-above reference mi (`a9611ea2`)

Site: `Neighbours::smooth_uv_neighbour_unsnapped` read the above chroma neighbour
at `(mi_r - 1, mi_c)` and the left at `(mi_r, mi_c - 1)`. libaom's
`set_mi_row_col` (`av1_common_int.h:1400-1401`) puts them at
`base_mi[-mi_stride + ss_x]` and `base_mi[ss_y*mi_stride - 1]`, where `base_mi`
is the block's own mi already de-offset by `(mi_row & ss_y)` / `(mi_col & ss_x)` —
which is exactly the snap the caller applies. Read as (row, column) deltas that
is `above = (-1, +ss_x)` and `left = (+ss_y, -1)`.

**Shipped: the above column.** It carries `+ss_x` wherever `ss_x == 1` — 4:2:0,
4:2:2 **and 4:4:0** all carry that column shift; only 4:4:4 (`ss_x == 0`) leaves
the read where it was. Measured: folding it in leaves the 4:2:0 control
byte-identical to `aomdec --rawvideo`. The honest statement of what that
measurement shows is that the boolean the column feeds came out the same on the
control's blocks — not that the shift is the identity there.

**Not shipped, and KNOWN UNRESOLVED: the left row term `+ss_y`.** libaom puts the
left reference one mi row DOWN at 4:2:0 and 4:4:0 (`ss_y == 1`) and on its own
row at 4:2:2 (`ss_y == 0`); this reads its own row at every subsampling. No
committed fixture presents a 1-mi-tall left neighbour (an 8x4 leaf's left block)
at 4:2:0/4:4:0, so nothing in the tree can witness the correct term today. This
is a named open discrepancy, not a settled one.

### How the split was chosen, and why 4:2:0 stays exact

An earlier attempt applied libaom's formula to BOTH reads (`above` row
`mi_r + ss_y - 1`, `left` row `mi_r + ss_y - 1`). That improved 4:2:2 (56142 →
53395) but regressed the byte-exact 4:2:0 control, and gating only the column
shift to 4:2:2 still regressed it — the `ss_y` term had been applied to the
WRONG read (the above), and that crossing is what the regression exposed. The
column term on its own is the part that measures clean; the row term is the part
that is still open, and shipping it is deferred to a lane that can witness it.

**Identity, measured three ways:**

1. 4:2:0 control `a420.obu` (same recipe at `--profile=0`) byte-identical to
   `aomdec --rawvideo` before and after both commits.
2. 4:4:4 `444_sb128rect_lr_witness.obu` output byte-identical to this branch's
   pre-change build (stash-and-rebuild) — `ss_x == 0` makes the column vanish.
3. Both pinned 4:2:2 witnesses still decode pixel-exact vs `aomdec`:
   `422_allskip_2f.obu` (2 frames, 128x128) and `422_sb128_3f.obu` (3 frames,
   128x128).

The column term was also A/B'd as 4:2:2-only versus all-subsamplings: identical
(53395) with the 4:2:0 control byte-exact either way, so the unconditional
libaom form ships — libaom's formula, not a 4:2:2 special case.

### Red/green

| measurement | before | after |
|---|---|---|
| `a1.obu` frame 0 PREFILT vs aomdec (diffing samples) | 56142 | **53395** |
| chroma 4x8 D67 block at chroma (88,72) = mi(18,44) | row0 `135,147,172,145` | **byte-equal to oracle `125,178,181,124`** |
| first 1419 TUs' prediction sums vs oracle | 1418 matched, #1419 mismatched | **1419/1419 match** |
| `a1.obu` all 16 frames vs aomdec | 1639654 diffs, 0/16 exact | 1619115 diffs, 0/16 exact |
| 4:2:0 control | byte-exact | byte-exact |
| 4:4:4 witness vs pre-change build | — | byte-identical |

The first divergent block is fixed outright. Confirmed offline against the spec:
with edge-filter strength 1 the prediction reproduces the oracle exactly (row0
`132,150,157,148`, col0 `132,140,147,154,159,157,155,149`); with strength 0 it
reproduces ours (`135,147,172,145`). The oracle printed `ft=1`, we computed 0.

## Defect 3 — PARTIALLY FIXED (`abae108a`), hand-off continues

**The first version of this section mis-attributed the class** (it called this
transform-size selection for a twice-as-tall plane block). Paired traces refute
that: see "What the class actually is" below.

### The measurement, on a clean single-frame stream

`a1.obu`'s frame 0 cut at the second OBU temporal delimiter, `a1_f0.obu`
(11802 B, decodes 1 frame on both sides). With both sides frame-scoped, two
independent pairings agree on the same boundary:

- **coefficient ladder** (`EC_TRACE_COEFF`, oracle `EC_COEFF_STEP` vs ours, paired
  on `all_zero` + post-read `rng`): units pair exactly through **1418**; unit
  **1419** is the first mismatch. Oracle 2512 units, ours 1959.
- **prediction ladder** (`EC_PREDOUT8` vs `OUR_PRED`, paired on sum + shape):
  same boundary, index **1419**.

```
1413 O mi(50,26) plane 0 '8x8' 5799   M '8x8' 5799   <- mi(50,26) block is FINE
1414 O mi(50,26) plane 1 '4x8' 3784   M '4x8' 3784
1415 O mi(50,26) plane 2 '4x8' 2468   M '4x8' 2468
1416 O mi(48,28) plane 0 '16x8' 16683 M '16x8' 16683
1417 O mi(48,28) plane 1 '8x8' 5504   M '8x8' 5504
1418 O mi(48,28) plane 2 '8x8' 9024   M '8x8' 9024
1419 O mi(50,28) plane 0 '8x8' 5715   M '4x4' 1424   <- FIRST divergence
1420 O mi(50,28) plane 1 '4x8' 3370   M '4x4' 1344
1421 O mi(50,28) plane 2 '4x8' 2513   M '4x4' 1488
```

Our unit 1419 averages 89.0 and the oracle's 89.3 over the same 8x8 footprint, so
the luma CONTENT agrees; only the tiling differs (one TX_8X8 vs four TX_4X4).

### What the class actually is: a MODE desync at mi(50,28)

`EC_TRACE_MODE_STEP` exists on BOTH sides and is directly comparable:

```
oracle mi(50,28): skip 35588  cdef 35588  dq 35588  mode val=0 41524
                  angle_y 0 41524  uv_mode val=4 60976  angle_uv val=3 50488
                  use_filter_intra val=1 38616   filter_intra_mode val=3 35858
ours   mi(50,28): skip 52484  cdef 52484  dq 52484  mode val=0 61324
                  angle_y 0 61324  uv_mode val=13 54978 angle_uv val=0 41424
                  use_filter_intra val=0 39456    (no filter_intra_mode read)
```

Same mi, same luma `mode` value (0), but **`uv_mode` 4 (oracle) vs 13 (ours)** and
a different `use_filter_intra` bit. The reader is already at a different state
when mi(50,28)'s `skip` is read, so the divergence is at or before it — this is an
ENTROPY desync, not a transform-size choice. The transform-shape difference is
downstream damage, not the defect.

What is **ruled out** for the desync:

- Not the tx_size lookup. Both sides read `tx_size_cdf[0]` at `ctx = 2` for the
  same 8x8 block (oracle `EC_TXCTXB mi=50,26 ... maxw=8 maxh=8 ctx=2`; ours
  `read_tx_size mi=(50,26) side=8 max_tx=8 ctx=2`), and `bsize_to_tx_size_cat`
  gives cat 0 for both BLOCK_8X8 and BLOCK_16X8 (`blockd.h:1344`, depth table
  index 1 and 3 both map to 0). Same row, same alphabet (2 symbols).
- Not `get_tx_size_context`. `pred_common.h:348-384` is `above + left` with the
  inter-neighbour override — the form `tx_size_context_txfm` already implements;
  both compute ctx 2 here.
- Not the frame header. `read_tx_mode` (`decodeframe.c:139`) and our
  `crates/ec-av1-syntax/src/frame.rs:1039` are the same function, and the header
  parses identically (the block modes before this point pair).
- Not the chroma sharing. mi(50,26) is even, so it owns the 4:2:2 chroma block and
  mi(50,27) shares it; all three of mi(50,26)'s units pair, and the oracle's
  `EC_IMODE` shows no block at mi(50,27).

**So: one symbol is consumed or skipped between mi(50,26)'s last unit and
mi(50,28)'s `skip`, and this lane did not identify which.** The oracle's
`EC_ISTEP` has no `tx_depth` print at all (it goes `angle_uv` straight to the
next block's `skip` at every site, including mi(48,26) and mi(48,28) where both
sides demonstrably stay aligned) — that is a rung-coverage gap, NOT evidence of
a missing read, and the `EC_ISTEP` line ORDER is not comparable between the two
builds (the oracle's trace jumps back from mi(50,26) to mi(48,28)), so the
step-by-step pairing cannot be used to bracket the extra read either.

### The bit-position bisect (run; narrows it, does not close it)

I added an exact bit position to the coefficient rungs on BOTH sides — ours via
the existing `SymbolDecoder::debug_bitpos()` (`msac.rs:520`), the oracle via
`aom_reader_tell(r)` in `decodetxb.c`'s `EC_COEFF_STEP` prints — rebuilt
`aomdec` with ninja, and paired the all_zero-level traces on bit position rather
than on the colliding 16-bit `rng`. Both rungs have since been reverted (the
oracle's `aomdec` is rebuilt to its committed shape; `strings aomdec | grep -c
bitpos` = 0).

The result is a clean constant, and it is the strongest statement this lane can
make about the desync:

```
unit          oracle bit    ours bit    delta
1414              56335        56349      -14
1415              56347        56361      -14
1416              56374        56388      -14
1417              56424        56438      -14
1418              56469        56483      -14     <- last unit that pairs
1419              56507        56527      -20     <- first divergent unit
```

The offset is a **constant -14** for every unit from 0 to 1418 — a fixed
convention difference between `debug_bitpos` and `aom_reader_tell`, nothing more.
At unit 1419 it becomes **-20**: relative to that baseline we are **exactly 6 bits
ahead**. So whatever the defect is, between unit 1418's `txb_skip` read and unit
1419's, we consume 6 more bits than libaom. That is a magnitude, and it is
bounded and small — not a whole skipped block or a whole mis-sized symbol read.

**Why this does not name the symbol.** I extended the same `bitpos` field to the
per-coefficient `tag=base` / `tag=br` prints to bisect inside the window, and
tried a bit-anchored greedy alignment of the two step streams. It does not work,
and the reason is worth recording: the two readers traverse a transform unit's
coefficients in DIFFERENT ORDERS (the oracle goes `read_coeffs_reverse_2d`, `c`
descending from `end_si`; ours emits `c=53, c=52, c=51` on what is plainly a
different unit), so their step streams have no index correspondence — 32500
oracle steps against 16464 of ours. Matching on bit position alone is ambiguous
and the alignment breaks within the first few hundred steps, long before the
window of interest. **A next lane must not trust a step-index pairing here.**

The workable refinement, which this lane did not have the budget to run: restrict
the comparison to ONE transform unit at a time (pair on the unit's own
coordinates, per the `ec-av1-oracle-trace-pairing` skill's "restrict pairing to
one frame/block" rule) rather than to the whole step stream. The 6-bit magnitude
says the target is very close — the same unit's tail, or the mode reads between
two units.

### The per-TU window bisect (run; the read is NAMED)

Extending `bitpos` to EVERY `EC_COEFF_STEP` rung on both sides and pairing
inside the single transform unit at index 1418 (its own coordinates, per the
`ec-av1-oracle-trace-pairing` rule) enumerates the window completely. Both
instrumentations are reverted again (oracle `aomdec` rebuilt, `grep -c bitpos` =
0).

**The window, 27 oracle reads against 28 of ours** (the extra one is a duplicate
PRINT of the same read, at the same bit, not a second read). Through the whole
base/br/Golomb loop the two sides are IDENTICAL — same tags, same order, same
values, same bit positions under the constant -14:

```
  O 0 bit=56469 all_zero plane=2 ... all_zero=0 rng=42420
  O 1 bit=56469 tx_type plane=2 txtype=0 txsize=1
  O 2 bit=56475 eob eob=8
  O 3..26  base/br/after_bases/sign/post_golomb  (c = 7,6,5,4,3,2,1,0)
  M 0 bit=56483 all_zero plane=0 ctx=2 entry=45064 all_zero=0 rng=42420
  M 1..21  eob/base/br/after_bases  (c = 7,6,5,4,3,2,1,0)
```

**The divergence is the first SIGN read.** Both decode the same value (1), but
from a different CDF row:

```
oracle  O 15  tag=sign c=0 sign=1 dcctx=1  rng=35769  bit=56491
ours    M 22  tag=sign_rect pos=0 sign=1 dcctx=0 rng=52678 bit=56506
```

`dcctx` — the DC sign context — is **1 on the oracle and 0 on ours**. Same symbol
value, different row, so the range coder leaves in a different state and every
later bit in the window shifts; that is the 6-bit excess, and it is why the
`rng` pairing that looked "identical through the base loop" turns over exactly at
the sign pass.

libaom takes that context from `txb_ctx->dc_sign_ctx` (`decodetxb.c:366`), which
is a **3-symbol read from `dc_sign_cdf_extra[plane_type]` taken once per
transform block** — a CDF that no `EC_COEFF_STEP` rung prints on either side, so
this lane could not pair that symbol and cannot say whether our value is wrong or
merely reflects an already-drifted `dc_sign_cdf_extra` adaptation state.

**Second, separate finding, worth its own look:** our scan table maps scan
indices to different raster positions than the oracle's for this unit — oracle
`c1 -> pos 8`, `c2 -> pos 1`; ours `c1 -> pos 1`, `c2 -> pos 8` (and
`c3 -> pos 2` vs ours `pos 16`, `c6 -> pos 24` vs ours `pos 3`). The coefficient
VALUES match by scan index on both sides, so this does not by itself move a bit,
but it places coefficients in the wrong cells and would corrupt the transform
independently of the entropy question.

### The dc-sign verdict: the VALUE is wrong, and the input is the rect path

Rung on both sides: ours is the existing `EC_DCDUMP` (which prints the vote and
every sampled neighbour cell), the oracle's is a new `DCSIGN` print in
`get_txb_ctx_general` reading `txb_ctx->dc_sign_ctx`, its `dc_sign` sum, and the
individual `a[]`/`l[]` sign fields over `txb_w_unit`/`txb_h_unit`. Both reverted
(oracle `aomdec` rebuilt, `grep -c DCSIGN` = 0).

**First, a wrong turn worth recording: our `dc_sign_ctx` table is CORRECT.**
libaom's `dc_sign_contexts` is 65 entries read at `dc_sign + 32`; extracted
programmatically it is `dc_sign < 0 -> 1`, `== 0 -> 0`, `> 0 -> 2`, which is
exactly what [`dc_sign_ctx`]'s `signum` buckets compute. A hand count of that
table literal suggests a different mapping and would have sent the next lane
after a non-bug; the table is fine.

**The vote differs.** libaom's own print at the neighbouring units is shaped
`dc_sign=-4 ctx=1 above=[-1,-1,-1,-1] left=[0,0,0,0,0,0,0,0]` — and the symbol
ladder showed the oracle resolving ctx **1** where we resolve **0**, i.e.
libaom's `dc_sign` is negative and ours is 0.

**The input, from our own `EC_DCDUMP` at the block in question:**

```
EC_DCDUMP mi=(48,28) plane=2 wh=(16,8) vote=0
  above=[None/6, None/6, Some(false)/7, Some(false)/7]   <- FOUR cells
  left =[Some(true)/7, Some(true)/7]                     <- TWO cells
```

`wh=(16,8)` is the block's LUMA footprint, so the rect path summed the vote over
**4 above + 2 left** cells. The unit actually being decoded is the 8x8 V CHROMA
transform, whose own `txb_w_unit`/`txb_h_unit` are **2 and 2** — libaom reads two
above cells, not four. Dropping two above cells can drop a negative contribution
and turn a negative vote into exactly the 0 we produce.

**So: the 4:2:2 chroma correction exists but is not wired into this path.**
[`around_mi_422_chroma`] — the function whose own comment says "libaom
`get_txb_ctx_general` reads its above votes over `txb_w_unit` CHROMA 4-px cells,
and at ss_x 1 one chroma cell spans TWO luma mi columns... Sampling every second
above cell counts each chroma column once, which IS libaom's sum" — is reached
1653 times on this stream, but NOT for this block: mi(48,28) prints
`EC_DCDUMP`, not `EC_DCDUMP422`. The **rect** path (`around_mi_rect`) has no 4:2:2
variant, so a 4:2:2 RECT block votes over the luma unit counts.

That is the same neighbour-read family as the `smooth_uv` fix, in the same file,
for the same reason: a per-axis shape correction that was applied to some paths
and not to the rect one. ****LANDED in `abae108a`.** `decode_leaf_rect`'s
`around_mi_rect` call was the one 4:2:2 chroma gather in the file still
ungated; it now mirrors the sibling gate (`chroma_422`, which that arm
already computes for its scan selection), luma keeping the per-mi gather:

```
before  EC_DCDUMP    mi=(48,28) plane=2 wh=(16,8) vote=0
        above=[None/6,None/6,Some(false)/7,Some(false)/7]   FOUR cells
after   EC_DCDUMP422 mi=(48,28) plane=2 wh=(16,8) vote=-1
        above=[None/6,Some(false)/7]                        TWO cells
```

Red/green: the bit offset is a clean constant through coefficient unit
1418 and turned 6 bits at 1419; after the fix unit 1418 pairs and the
first divergence moves to **2427**. Frame-0 coefficient units 1959 →
2680 (oracle 2512). Whole stream, 16 frames vs `aomdec --rawvideo`:
**1619115 → 1380482** differing samples, still 0/16 exact.

Identity, four ways, all unchanged: 4:2:0 control `a420.obu`
byte-identical to aomdec; 4:4:4 `444_sb128rect_lr_witness.obu`
byte-identical to the pre-lane build; pinned `422_allskip_2f.obu` and
`422_sb128_3f.obu` still pixel-exact. The gate is ss (1,0), so the sampler
is not reached at 4:2:0 or 4:4:4 at all.

### Round 7 — the mi(64,24) boundary localized to a PARTITION/bsize decision

Paired on `a1_f0.obu` with the round-6 fix in place. The boundary is a
**block-structure** decision, not a transform lookup and not another
neighbour vote:

```
oracle  EC_IMODE mi_row=64 mi_col=24 bsize=9                 (BLOCK_64X32)
        -> codes a 32x32 luma TU (TU 2428, sum 174006), chroma 16x32
ours    EC_IMODE mi_row=64 mi_col=24 fn=sq side=32           (a 32x32 SQUARE)
        EC_ISTEP ... name=tx_depth val=2 ctx=1               -> 32 >> 2 = TX_8X8
        -> codes 8x8 leaves (sum 10816), chroma 8x8
```

So at the same mi the oracle walks a 64x32 rect and we walk a 32x32
square — the PARTITION symbol, not `tx_depth`, is where the two decoders
part company. The block sits on the frame's last superblock row (288
tall, SB rows at 0/128/256, the last only 32 rows), inside SB(mi_row 64,
mi_col 0).

**Where the reader actually parts.** Both step traces already differ at
that block's FIRST symbol (`skip`, oracle rng 39524 against ours 40716),
so the desync precedes it; and the coefficient ladder pairs through unit
2427, which is mi(68,16)'s V chroma. The window is therefore the reads
between mi(68,16)'s chroma tail and mi(64,24)'s mode read — not the
tx_size lookup (the oracle's `get_tx_size_context` for that block is
printable and ours computes the same `above + left` form as everywhere
else).

### Round 19 — the tail pairing FAILED; the poisoning read is NOT named

All instrumentation reverted; `aomdec` restored (zero stray strings,
2995 `EC_COEFF_STEP` lines, 4:2:0 control byte-exact). Build discipline
followed: touch before ninja, rung verified before use.

**What was set up.** Same-line bit positions on every coefficient rung
on both sides — 13 sites here, 11 in the oracle's `decodetxb.c` — and
verified (75970 rung lines with the field live). So round 18's
instrument is reproducible and the walk is no longer blocked by
emission.

**What failed, and why this lane stops here.** Pairing the mi(68,16)
tail by bit position does not work, and the failure is structural:

```
  O bit=92304 all_zero  plane=2   |  M bit=92331 eob
  O bit=92315 all_zero  plane=0   |  M bit=92332 base
  O bit=92316 all_zero  plane=1   |  M bit=92332 after_bases
  O bit=92316 tx_type   plane=1   |  M bit=92333 sign_rect
```

The two rung streams have **different granularity**: this decoder
prints one line per transform unit with a different tag set, the oracle
one line per coefficient step with another. Bit-position inference
inside a single unit's tail therefore pairs UNRELATED reads. The 1:1
correspondence that made rounds 10 and 14 work came from the
`all_zero` ANCHOR — one per transform unit, on both sides. **Inside a
unit there is no such anchor**, and this round had none.

**Consequence for the earlier attributions, stated precisely.** The
round-10 "3 bits short" and the round-14 window were taken at
`all_zero` granularity, which IS reliable, and their conclusion (the
first-delta read is the luma mode at mi(64,24)) stands. Anything finer
than one transform unit was never established.

**A lead withdrawn.** The round-18 "the oracle emits a `tx_type` step
at mi(68,16) where our rungs emit none" is NOT evidence. Our
`tag=tx_type` prints sit at two sites; the oracle's print is per plane.
The 2080-against-1033 count difference is rung coverage, and the
absence of a line here is an instrumentation gap. Withdrawn.

**The instrument that would close it.** A per-READ trace emitted from
the symbol decoder ITSELF on both sides — our `SymbolDecoder::symbol`
and `literal`, libaom's `aom_read_symbol` — carrying an explicit read
SEQUENCE NUMBER alongside (row, decoded value, post-read rng, bit
position). With a shared counter the two streams are 1:1 **by
construction** rather than by bit-position inference, and the first
read whose rng diverges names the poisoner directly, inside whatever
block it falls. That is a real round of work and it is not a
continuation of this one.

### Round 18 — the table did NOT drift; the coder state already had

All instrumentation reverted; `aomdec` restored (744 `name=mode` lines,
zero stray strings), 4:2:0 control byte-exact. Build discipline
followed: touch before ninja, known-firing rung verified before any new
field was read.

**1. The `kf_y_mode` row immediately BEFORE the mi(64,24) read is
IDENTICAL on both sides.** (Our storage is `32768 - cdf`; converted, the
twelve entries are the same twelve numbers to the digit:)

```
oracle  20236, 19127, 17626, 16471, 13708, 11379, 9074, 7617, 6502, 1079, 805, 127
ours    20236, 19127, 17626, 16471, 13708, 11379, 9074, 7617, 6502, 1079, 805, 127
```

**So the table-state verdict is CLEAN — it did not drift.** That kills
the round-17 hypothesis and answers Main's step 2 negatively: there is
no read whose adaptation update differs.

**2. But the range coder is ALREADY in a different state at that
read**, at the same bit position (the constant -14):

```
oracle  KFPRE rng=39524  dif=833299967  bitpos=92323
ours    KFPRE rng=40716  val=24467       bitpos=92337
```

Same CDF row, same bit position, different `rng` — therefore a
different decoded value (1 against 6) and a different bit cost (4 bits
against 5). The decode itself is innocent; the STATE is the defect.

**3. mi(64,24) is the FIRST such block.** Across all 742 mode reads
common to both traces, every block before it agrees in BOTH its decoded
mode and its post-read `rng`; mi(64,24) is the first that differs, and
the 18 after it are downstream. Its very first read (`skip`) is already
divergent, so the cause is in the PREVIOUS block's tail (mi(68,16)) or
in mi(64,24)'s own pre-mode reads.

**This is the round-6 dc-sign shape, one level up.** Every read in that
window consumes the SAME BITS on both sides, and the bit delta is
constant — but a symbol read from a different CDF row (or the same value
decoded from a different row) consumes the same bits and leaves a
different `rng`. The ladder cannot see it; only a state comparison can,
and this is the first place the state comparison has been run.

**4. Prime suspect, with its evidence and its limits.** The ladder's
window shows the oracle emitting a `tx_type` step at mi(68,16) where our
rungs emit none. But the raw counts are oracle 2080 `tx_type` reads to
our 1033 across the frame, and that is a RUNG-COVERAGE difference (our
`tag=tx_type` prints sit at two of several sites), **not** evidence of a
missing read. Recorded as a lead, not a finding.

**Unblock.** Pair, bit-position-anchored, the pre-read CDF ROW and the
decoded VALUE for every read in the mi(68,16) tail — the same
instrument that produced this verdict, applied one block earlier. The
first read whose row or value differs names the root. With the
instrument now proven (it produced a clean, digit-exact table match and
a clean state mismatch), that run is mechanical.

### Round 17 — MEASURED: the bands MATCH, and round 15's mystery is solved

Build discipline first: `touch` before `ninja`, and the known-firing rung
verified at **744 `name=mode` lines** before any new field was trusted.
All instrumentation reverted; `aomdec` restored (744 lines, zero stray
strings), 4:2:0 control byte-exact.

**Round 15's mystery, solved.** The `MODECTX` rung there did not
"fail to fire" — it fired, printed for the first block, and then the
**process died**. `above_mi` is NULL for a block on the frame's first
row (and `left_mi` on its first column), and the unguarded
`above_mi->mode` dereference killed the decode, so nothing after that
one line was ever emitted. A `MODEMARK` printed BEFORE the field access
made it unambiguous: 1 marker, 0 mode lines. Guarded
(`above_mi ? above_mi->mode : -1`), the extended print works and the
rung returns to 744. **Round 15's conclusion was an artefact of a NULL
dereference, and is now closed rather than left as a mystery.**

**The band-match verdict: THEY MATCH.** With the guard, the oracle at
mi(64,24) reports

```
EC_ISTEP mi_row=64 mi_col=24 name=mode val=6 rng=44416 above_mode=0 left_mode=6
```

against ours `above_mode=0 left_mode=6 above_ctx=0 left_ctx=4 -> mode=1`.
Compared across the WHOLE frame — all 744 mode reads, ours from all
three `kf_y_mode` sites (square, rect, sub-8) — the contexts are
identical up to and including mi(64,24) (oracle mode-read index 723 of
744). The 37 raw differences are all libaom's NULL neighbour (`-1`)
against our `DC_PRED`, which `get_y_mode_cdf` maps to the same context;
after that mapping 17 remain, and **every one of them is at index >= 724,
i.e. strictly after the divergence**. The first differing context is the
first one downstream of it.

**So: not a neighbour-lookup defect.** The lane-rectx r5 mi-exact
override hypothesis is **ruled out** for this block — the square path's
coarse bands carry exactly what libaom's `above_mbmi` / `left_mbmi`
carry here. (Whether the square path *should* use the mi-exact map
generally is a separate question this block does not answer.)

**Where that leaves the defect.** Same context, same bit position,
different symbol value (1 against 6) — so the divergence is the
`kf_y_mode[0][4]` **table state**, not the row. AV1's
`aom_read_symbol` adapts backwards as well as forwards, so a read from a
NEIGHBOURING row can move `[0][4]` without moving any bit position, and
the ladder cannot see that. Main's step 4 is therefore the live
question and the one worth the next round.

**Unblock.** Print `kf_y_mode[0][4]` (and the whole `kf_y_mode` table, or
just that entry) immediately BEFORE the mi(64,24) mode read on both
sides. If the entry already differs, the drift is upstream and the
first read that moved it is findable by walking the table backwards
across the 723 preceding mode reads. If the entry agrees and only the
decoded value differs, the defect is in the symbol decode itself, not
in the table — a much narrower target.

### Round 16 — the unblock was NOT completed, and a build hazard was found

No code change; `aomdec` restored to its committed shape
(`grep -cE 'MODECTX|bitpos'` = 0, 744 `name=mode` lines again, 4:2:0
control byte-exact).

**What happened.** The instruction was to extend the KNOWN-FIRING
`EC_ISTEP name=mode` print with `above_mi->mode` / `left_mi->mode`
rather than add a sibling `getenv` rung. That edit was made, the string
confirmed in the binary — and the rebuilt `aomdec` then emitted **zero**
`name=mode` lines, i.e. the instrumented decode stopped producing the
rungs entirely. Reverting the print and rebuilding restored 744 lines.
**The oracle-side context is therefore still unmeasured**, and this lane
does not report a value it did not read.

**The build hazard, which is the durable finding here.** `ninja` in
`~/.cache/aom-oracle/build` does **not** reliably rebuild after a source
edit: repeatedly it reported only `[1/1] Updating version info if
necessary.` and left the binary stale, with the binary's mtime older
than the source's. A `touch` on the edited source is required before
`ninja aomdec`, and the result must be confirmed by running the binary
and checking a known-firing rung — not by `strings` alone.

**This bears directly on round 15.** Round 15 concluded that a
`MODECTX` rung "does not fire" because it was placed immediately above
`mbmi->mode = read_intra_mode(...)` in the same function as a rung that
fires 744 times. Given this build behaviour, that conclusion is unsafe:
the rung may not have been compiled at the moment it was tested, or the
binary may have been in the same partial state seen this round. Round
15's own evidence (`strings` finding the format string in the binary)
argues it was compiled, so the mystery is unresolved — but the
conclusion "the oracle does not print it" should be treated as
UNVERIFIED, not as a property of the decoder.

**Standing state of the mode-context question.** Ours is measured:
mi(64,24), `above_mode=0 left_mode=6 above_ctx=0 left_ctx=4`, `mode=1`
against the oracle's `mode=6`. The oracle's `above_mi->mode` /
`left_mi->mode` are still unknown, so **the band-match verdict is not
answered** and the lane-rectx r5 override is neither confirmed nor ruled
out. Main's step 4 (the CDF-drift question) is likewise untouched: if
the bands turn out to MATCH the cells, the next thing to check is
whether the `kf_y_mode` table state itself drifted at an earlier read,
and the ladder is now trustworthy enough to answer that.

**Unblock, restated with the build caveat.** `touch
~/.cache/aom-oracle/src/av1/decoder/decodemv.c`, rebuild, then verify
the binary still emits its 744 `name=mode` lines BEFORE reading any
extended field. Then compare against mi(63,24) / mi(64,23) on our side.

### Round 15 — the mode context: OUR side measured, the ORACLE side would not print

Both prints reverted; `aomdec` rebuilt to its committed shape
(`MODECTX` = 0).

**Our side, at mi(64,24), from the square `read_intra_mode`** (the path
this block actually takes — the `read_intra_mode_rect` site does not
fire for it, and neither does `read_intra_mode_sub8`):

```
MODECTXM mi=(64,24) above_mode=0 left_mode=6 above_ctx=0 left_ctx=4
            -> mode val=1        (oracle: mode val=6)
```

**The oracle side could not be measured, and this lane will not guess
past that — SOLVED in round 17: the rung fired and the unguarded
`above_mi->mode` dereference killed the decode at the frame's first
row, so nothing after the first line was ever emitted. Guarded, the
extended print works. Round 15's "does not fire" was a NULL
dereference, not a property of the decoder. A `MODECTX` rung placed immediately above
`mbmi->mode = read_intra_mode(r, get_y_mode_cdf(ec_ctx, above_mi,
left_mi))` in `decodemv.c:936`, rebuilt, with its format string
confirmed present in the binary and the env var set, emits nothing —
while the `EC_ISTEP name=mode` print three lines below it in the SAME
function fires 744 times, so the code provably runs. One occurrence of
the site in the source, in `ec_read_intra_frame_mode_info_impl`. Not
resolved.

**What is established.** The divergence is the
`kf_y_mode[above_ctx][left_ctx]` row: same block, same bit position,
different symbol VALUE (1 against 6). Our square reader takes
`above_mode` / `left_mode` from the **coarse** `above_mode[c]` /
`left_mode[r]` bands, whereas the rect path goes through
`modes_above_left_mi` (the mi-exact map, lane-rectx r5's override) and
libaom's `get_y_mode_cdf` uses `xd->above_mbmi` = `mi[-mi_stride]` =
mi(63,24) and `xd->left_mbmi` = `mi[-1]` = mi(64,23). So there is a
concrete question — does the square path's coarse band hold what
mi(63,24) / mi(64,23) hold — and this lane did not get to answer it.

**A coincidence recorded as a lead, not a claim.** Our `left_mode=6`
at mi(64,24) is the same number the oracle DECODES as that block's
mode. That is what a neighbour-cell mix-up would look like, and it is
the shape Main predicted (the rect intra-mode path's own unsnapped
lookup). It is one data point and is not evidence.

**Unblock, and it is short.** Print `above_mi->mode` / `left_mi->mode`
from the `EC_ISTEP name=mode` site itself — the print that is known to
fire — rather than from a sibling `getenv` rung, and compare against
mi(63,24) / mi(64,23) on our side. If the square path's coarse bands
differ from those cells, the fix is lane-rectx r5's mi-exact override
applied to the square `read_intra_mode` path, the same way the rect
path already has it — the same neighbour-context family as the
`around_mi_422_chroma` and dc-sign fixes, and the third instance of it.

### Round 14 — the ladder walked: the first-delta read is the LUMA `mode` at mi(64,24)

Ladder rebuilt with the round-13 emission fix on both sides (position
appended before the format string's `\n`; 14 `EC_ISTEP` + 11
`EC_COEFF_STEP` sites in the oracle, 18 coefficient + 2 mode-macro sites
here), then reverted (`grep -c bitpos` = 0, `MODECTX` = 0).

Instrument note, earned the hard way twice already: our `EC_ISTEP`
positions come from the `istep!` macro and our `EC_COEFF_STEP` from the
per-rung prints, and a strict index walk between the two sides breaks
immediately because the two rung SETS differ per path (the oracle emits
a `tx_type` step this decoder's rect reader has no print for). The
ladder is therefore walked on the **all_zero anchors** — the rung both
sides emit once per transform unit — and the delta map is read off the
bit positions in between.

**The delta map.** Constant **-14** (ours 14 bits ahead) for every
transform unit from 0 to 2427, changing to **-11** at unit 2428. Inside
that window, with `+14` folded in and every oracle read checked for a
counterpart on our side:

```
  92321  all_zero  plane=2   OK
  92323  skip       mi(64,24)  OK
  92323  cdef       mi(64,24)  OK
  92323  dq         mi(64,24)  OK
  92327  mode       mi(64,24)  <-- first read with NO counterpart
  92330  angle_y    mi(64,24)  (re-paired, shifted)
  ...
  92339  all_zero   <-- delta now -11
```

**The named read is the luma `mode` symbol of mi(64,24).** Oracle
`mode val=6`, ours `mode val=1`. Cost: the oracle's read spans 92323 →
92327 (4 bits), ours 92337 → 92342 (5 bits). Every read before it pairs
exactly at +14; the deviation begins at this one and persists to the
window's end, which is where the -14 → -11 step in the unit ladder
comes from. The same block then carries the round-7 structural
consequence: one 32x32 luma TU (oracle) against 8x8 leaves (ours).

**What is not yet named, stated plainly.** Same block size and same
bit position at the read, different symbol VALUE — so the divergence is
in the mode CDF ROW, i.e. the `kf_y_mode[above_ctx][left_ctx]` context.
This lane tried to print that context on both sides and did not get a
paired measurement: our block reaches `read_intra_mode_rect`, not the
square `read_intra_mode` the print was placed in, and the oracle-side
`MODECTX` rung did not fire under its env var in the time available.
Both prints are reverted. **No fix is claimed.**

**Unblock.** Print the mode context at mi(64,24) on both sides — ours
from `read_intra_mode_rect` (the path that block actually takes), the
oracle's `get_y_mode_cdf(ec_ctx, above_mi, left_mi)` — and compare
`above_mode` / `left_mode` at mi(64,24). The unit ladder is now a
trustworthy instrument, so once the context is paired the fix follows
directly and the ladder re-run confirms it.

**Credit where it is due.** Round 13's instrument correction — moving
the bit position onto the same physical line as its read — is what made
this round possible. Rounds 11 and 12 both reported findings that were
artefacts of that placement; without the fix this window would have
been "analysed" a third time and reached a third wrong conclusion.

### Round 13 — SETTLED: the oracle DOES consume them, and our reads match exactly

**Outcome (a).** The rung emission is fixed — the bit position is now
appended BEFORE the format string's `\n`, so it lands on the same
physical line (all 18 `EC_ISTEP` and 14 `EC_COEFF_STEP` sites in
`decodemv.c` / `decodetxb.c`; `aomdec` rebuilt, all reverted again,
`grep -c bitpos` = 0).

With correct emission, the oracle at mi(68,16):

```
angle_uv          val=-2  rng=57128  bitpos=92312
use_filter_intra  val=1   rng=37468  bitpos=92312
filter_intra_mode val=3   rng=35632  bitpos=92313
next block mi(64,24) skip           bitpos=92323
```

and ours, from the same trace:

```
angle_uv          val=-2             bitpos 92312
use_filter_intra  val=1              bitpos 92312
filter_intra_mode val=3              bitpos 92313
next block mi(64,24) skip           bitpos 92323
```

**Identical — same values, same bit positions, same next-block
position.** The oracle consumes both symbols; so do we; the reads
agree. This window is CLOSED and the two extra reads that rounds 11 and
12 chased never existed.

**Why rounds 11 and 12 saw them anyway — the mechanism, recorded so it
is not repeated.** With the position on the previous line, a
record-wise parse (split on `EC_ISTEP`, read `bitpos=` from inside the
record) attributes each read's position to the PRECEDING read, because
that is the line it was physically printed on. So the two
`use_filter_intra` / `filter_intra_mode` reads carried the bit positions
of the reads before them, and a window bounded on those positions
silently excluded them. The lesson is sharper than round 8's: **with a
multi-field rung, a positional parse is only as good as the field's
placement** — and a field that lands on an adjacent line does not
announce itself, it just quietly misattributes.

**What the round-10 "3 bits short" number now means.** It stands as a
measurement — the coefficient-unit bit delta is constant -14 through
unit 2427 and changes at 2428 — but the window attributed to it does
NOT contain the divergence: the mode reads in it are now proven to
match symbol for symbol and bit for bit. The three bits are elsewhere
in that span, or the all_zero-level delta change at 2428 has a cause
this lane has not reached. **This lane ran out of budget before
re-anchoring the ladder past mi(68,16) to find the real first-delta
read**, which is the next step and is unchanged from round 12's option
(a) branch.

### Round 12 — CORRECTION to round 11: the extra reads are real, the CAUSE is not what round 11 said

Round 11 was wrong twice over, both times from trusting a print
without checking how it is emitted. Recorded here rather than left
standing.

**Error 1, a parse artefact.** The oracle's `EC_ISTEP` lines carry
`bitpos=` but the insertion point placed it after the format string's
`\n`, so the position lands on the PREVIOUS line. Round 11's
line-filtered window therefore dropped every oracle `use_filter_intra` /
`filter_intra_mode` line it was looking for. Re-parsed RECORD-WISE
(splitting on `EC_ISTEP` and reading the bit position out of the record
rather than the physical line), the oracle emits 241 `use_filter_intra`
lines and the two sides read:

```
ORACLE  [92304] skip/cdef/dq   [92305] mode 0  angle_y 0
        [92309] uv_mode 4      [92312] angle_uv -2
        [92323] skip  <- next block, no filter_intra in between
OURS    [92304] skip/cdef/dq   [92305] mode 0  angle_y 0
        [92309] uv_mode 4      [92312] angle_uv -2
        [92312] use_filter_intra  val=1
        [92313] filter_intra_mode val=3
        [92323] skip
```

So the OBSERVATION survives the correction: at mi(68,16) we make those
two reads and the oracle shows none in that span.

**Error 2, and this one kills round 11's conclusion.** Round 11 said
libaom's `av1_filter_intra_allowed` should have suppressed them and that
the suppressing input was `enable_filter_intra` or `palette_size[0]`.
Measured directly, with a rung inside libaom's own
`read_filter_intra_mode_info`:

```
PALSZ mi=68,16 bsize=8 mode=0 psize0=0 efi=1 wide=32 high=16 allowed=1
```

**`allowed=1`.** libaom's predicate is SATISFIED at that block — mode is
DC_PRED (0), `palette_size[0]` is 0, `enable_filter_intra` is 1, and the
32x16 footprint passes `wide <= 32 && high <= 32`. So libaom DOES read
`use_filter_intra` there, and round 11's "the read should not have
happened" framing is wrong. The two unmeasured inputs were measured and
BOTH AGREE with us; there is no stale palette state and no header-bit
disagreement.

**What is left, stated honestly.** Our two extra reads are real in the
bit stream, and libaom's own predicate says the corresponding reads are
allowed — so the divergence is NOT a missing/extra gate in the sense
round 11 described, and this lane cannot say what it is. The one
remaining tension is internal to the instrumented oracle: its
`allowed=1` and its absent `use_filter_intra` EC_ISTEP at mi(68,16) do
not sit together, which points at the ORACLE's instrumentation (the
`EC_ISTEP` print is inside the `if`, and something about that path is not
reaching it) rather than at a decoder defect. **The next lane should
re-verify the oracle side before touching ours** — on the strength of
rounds 8, 11 and 12, this tree has twice rewarded a conclusion that a
single print, read without checking its emission or its guard,
contradicted.

**Unblock, restated.** Do NOT gate our `use_filter_intra` read. First
settle the oracle: does an unmodified-ish `aomdec` consume the
`use_filter_intra` symbol at mi(68,16)? Compare its bit position after
`angle_uv` with aomdec's mode-info trace, with the print placed so its
bit position is on the SAME line (append the field before the `\n`).
Only if the oracle genuinely does not consume it is our read the defect,
and then the predicate that would have to differ is not
`av1_filter_intra_allowed` — it is whatever else gates the call.

### Round 11 — the two extra reads are NAMED: `use_filter_intra` + `filter_intra_mode`

Bit positions added to the MODE rungs on both sides (our `EC_ISTEP`
per-symbol macro, the oracle's twelve `EC_ISTEP` prints in
`decodemv.c`), paired BIT-POSITION-ANCHORED throughout, then reverted
(`grep -c bitpos` = 0).

The window, with the constant −14 folded in (oracle bit in brackets):

```
        mi(68,16)                                        mi(64,24)
oracle  92304 skip/cdef/dq  [92304]
        92305 mode 0  [92305]   angle_y 0  [92305]
        92309 uv_mode 4  [92309] angle_uv -2 [92312]
        ---- block ends ----                          92323 skip/cdef/dq
ours    92318 skip/cdef/dq  [92304]
        92319 mode 0  [92305]   angle_y 0  [92305]
        92323 uv_mode 4  [92309] angle_uv -2 [92312]
        92326 use_filter_intra val=1   <-- EXTRA
        92327 filter_intra_mode val=3  <-- EXTRA
                                                   92337 skip/cdef/dq
```

**The two extra reads are at mi(68,16), and the oracle does not make
them.** Its silence is not a rung gap: the oracle's `use_filter_intra`
print sits inside `if (av1_filter_intra_allowed(cm, mbmi))` and its
`filter_intra_mode` print inside `if (...use_filter_intra)`
(`decodemv.c:638-656`), so a missing line means the read did not happen.

**The block is the same on both sides** (enum-resolved, per the round-8
lesson): oracle `EC_IMODE mi_row=68 mi_col=16 bsize=8`, and
`enums.h:100-115` makes 8 = **BLOCK_32X16**; ours reads
`EC_IMODE ... fn=rect bw=32 bh=16`. Same footprint, same bit position.

**Which gate should have suppressed them** —
`av1_filter_intra_allowed` (`reconintra.h:75-79`) is

```
mbmi->mode == DC_PRED && palette_size[0] == 0
            && (enable_filter_intra && block_size_wide <= 32 && block_size_high <= 32)
```

The block satisfies the mode (0 = DC_PRED) and size (32/16, both <= 32)
conditions, so the suppressing input is one of the two this lane did NOT
measure: the sequence header's `enable_filter_intra`, or the block's
`palette_size[0]`. **This lane ran out of budget before measuring
either, and does not claim which.**

**Unblock, now short.** Print `enable_filter_intra` and
`palette_size[0]` for this block on both sides and compare against the
predicate above. If `enable_filter_intra` agrees, the defect is our
`palette_size[0]` state for the block; if it disagrees, it is the
sequence-header bit. Either way it is a single boolean, and the fix is
one gate on the `use_filter_intra` read.

### Round 10 — the second window: 3 bits, and they are in the MODE reads

`EC_COEFF_STEP bitpos` re-added on both sides (18 rungs here, 11 in the
oracle's `decodetxb.c`), `aomdec` rebuilt, all reverted again
(`grep -c bitpos` = 0).

The bit delta is the same constant **-14** for every coefficient unit
from 0 through 2427, and changes at **2428**: the oracle consumes 18
bits across that unit, we consume 15. **We are 3 bits short** — the
mirror image of round 6, where we were 6 bits long.

The window, enumerated (both units are all_zero=1 skips, so no
coefficient reads fall inside it):

```
oracle  92321  all_zero plane=2 ctx=8  all_zero=1 rng=60208
        92339  all_zero plane=0 ctx=0  all_zero=1 rng=38996     (+18 bits)
ours    92335  all_zero plane=0 ctx=1  all_zero=1 rng=61576
        92350  all_zero side=8 ctx=3   all_zero=0 rng=48464     (+15 bits)
```

**What this does and does not establish.** It establishes the MAGNITUDE
and the SIDE: 3 bits, consumed by block-mode reads, not by any
coefficient read — the `EC_COEFF_STEP` stream shows only the two skip
units, so the three bits live in the mode/mode-preface reads
(`skip`, `cdef`, `delta_q`, `mode`, `angle_delta`, `uv_mode`,
`angle_delta_uv`, `use_filter_intra`, `filter_intra_mode`) that no rung
on either side currently carries a bit position for.

**A trap recorded, per the round-8 lesson.** The unit INDEX pairing is
only valid while the bit delta is constant, and it stops being a safe
guide at exactly this point: our decoder emits 2680 coefficient units
against the oracle's 2512, and a skipped unit's `txb_skip` read can
cost as few as one bit, so extra skip units can coexist with a constant
bit delta. Note in the window above that the oracle's unit 2427 is
`plane=2` and ours is `plane=0` — the indices no longer name the same
block even though the bit positions still agree. **Pair bit positions,
never unit indices, from here on.**

**Enum discipline applied to what IS printed here.** `plane=0/1/2` is
libaom's `PLANE_TYPE_Y/U/V` ordering and matches ours; `side=8` on our
line is the square `reconstruct`'s side, i.e. an 8x8 transform unit;
`ctx` on both is the `txb_skip` context, not a tx_size context (the
tx_size context is the separately printed `EC_TXCTXB`/`tx_depth ctx`).
Nothing in this window is claimed to be a bsize, a partition, or a
tx_size category, so no enum lookup is needed to read it.

**Unblock.** Add a bit position to the MODE rung on both sides — the
`EC_ISTEP` line ours already prints per symbol, and the oracle's
equivalent in `decodeframe.c`'s intra mode-info read — and pair the eight
mode reads across this window. Three bits across seven or eight reads is
a single short read (most plausibly a `filter_intra_mode` or a
`use_filter_intra` the oracle takes and we do not, or a cfL alpha read
present on one side only), and a per-symbol bit position will name it
directly. That is another instrumentation round and is left unscheduled
here.

### Round 9 — CORRECTION to round 8, and where the desync actually starts

**Round 8's central claim was wrong and is retracted here.** It read the
oracle's `PARTB bsize=9` as `BLOCK_64X32`. The real enum
(`enums.h:100-115`) is
`0 BLOCK_4X4, 1 BLOCK_4X8, 2 BLOCK_8X4, 3 BLOCK_8X8, 4 BLOCK_8X16,
5 BLOCK_16X8, 6 BLOCK_16X16, 7 BLOCK_16X32, 8 BLOCK_32X16, 9 BLOCK_32X32,
10 BLOCK_32X64, 11 BLOCK_64X32, 12 BLOCK_64X64, 13 BLOCK_64X128,
14 BLOCK_128X64, 15 BLOCK_128X128`. So `bsize=9` is **BLOCK_32X32** —
the SAME block size we decode. The oracle is not walking a 64x32 where we
walk a 32x32; both are at BLOCK_32X32. Round 8 also mis-called `bsize=15`
a 64x16 (it is BLOCK_128X128, the superblock root) and `bsize=12` — it is
BLOCK_64X64, not BLOCK_128X64. The rest of round 8 stands: the
partition reads pair, bottom-edge truncation is ruled out, and the
oracle's lineage is `15 (128X128) -> 12 (64X64) -> 12 (64X64) ->
9 (32X32) -> 9 (32X32)`.

**What round 9 establishes, with the enum right this time.** At
mi(64,24) both decoders are on a 32x32 block and read the same tx_size
row at the same context:

```
oracle  EC_TXCTXB mi=64,24 bsize=9 maxw=32 maxh=32 abv=32 lft=16 above=1 left=0 ctx=1
ours    EC_ISTEP mi_row=64 mi_col=24 name=tx_depth val=2 ctx=1
```

`bsize_to_tx_size_cat(BLOCK_32X32)` is 2, and our `max_tx == 32` arm
reads `tx_size_cat2[ctx]` with `ctx = 1` — the same CDF row the oracle
uses, at the same bit position (the -14 constant still holds). So the
tx_size lookup is NOT the divergence either.

**Where the desync actually starts.** The reader state already differs at
mi(64,24)'s FIRST symbol (`skip`, oracle rng 39524 against ours 40716),
and the coefficient ladder pairs through unit 2427. So the window is the
reads between unit 2427's block and mi(64,24)'s `skip` — the same
narrow "between two units" window round 6 characterised, now at its
second occurrence. Both this window and round 6's were closed by pairing
per transform unit; this one is still open.

**Corrected summary of what is known at mi(64,24):** same bsize, same
tx context, same CDF row, same bit position — and still a different
outcome (oracle one 32x32 luma TU, us 8x8 leaves), with the reader
already out of step at the block's first read. The structural difference
is real and downstream; the DESYNC is upstream of it and is the thing
still to find.

### Round 8 — the partition reads PAIR; the block size entering them does not

Rungs added and reverted on both sides (ours `EC_TRACE`, the oracle a `PARTB`
print in `ec_read_partition_impl` with `has_rows`/`has_cols`/`ctx`/bit
position/value; `aomdec` rebuilt, `grep -c PARTB` = 0).

Bottom-edge truncation is **ruled out**: libaom recomputes
`has_rows = (mi_row + hbs) < mi_rows` with the CURRENT bsize's `hbs`
(`decodeframe.c:1292`), and so does ours, per level — both go
`has_rows=0` at the SB and the 64 level and `has_rows=1` from the 64x32
level down. The edge arithmetic agrees at every level of SB row 64.

**The partition reads themselves are identical.** Bit positions carry the
same constant −14 right through, and the values agree:

```
        oracle                                          ours
mi=64,16 bsize=9 ctx=8  bitpos=92247  value=1     92261  value=1
mi=64,24 bsize=9 ctx=10 bitpos=92321  value=0     92335  value=0
mi=64,32 bsize=9 ctx=9  bitpos=92356  value=3    93518  value=2   <- DIVERGED
```

At mi(64,32) the oracle is at bit 92356 and we are at 93518 — **1162 bits
later**. The gap opens in the block that FOLLOWS mi(64,24): the oracle
spends 33 bits there (92323 -> 92356) and we spend ~1183.

**RETRACTED — see round 9 below.** This section read the oracle's `bsize=9`
as `BLOCK_64X32` and concluded the block size entering the read was the
divergence. With the real enum (`enums.h:100-115`) `bsize=9` is
`BLOCK_32X32`, the same size we decode, and that conclusion is wrong.
What survives from round 8: the partition reads pair, and bottom-edge
truncation is ruled out.

**What is left to do, and why it is not another step here.** Tracing the
bsize lineage from the SB root down to mi(64,24) on both sides — the
`PARTB bsize=` field at each level of SB row 64, which the oracle's rung
prints and ours does not — is a further instrumentation round, not a
continuation of this one. The oracle's own trace already shows the shape
to chase (`mi=64,0 bsize=15 -> bsize=12 -> mi=64,16 bsize=12 -> bsize=9`
with `has_rows` flipping 0 -> 1 at the 64x32 level), and the question is
which of those levels our decoder resolves to a different bsize.

**Scan table, still open.** Unchanged and not yet re-checked: it cannot
be re-checked until the partition tree is right, since the tiling it was
observed under is decided by the same walk.

**Why this round stops here rather than continuing.** The partition rungs
this lane has — ours `EC_TRACE_PART`, the oracle's `AOMMB` — do not fire
for these blocks at all, and the bit-position rungs used in rounds 5-6 are
reverted. Closing this needs a fresh instrumentation round aimed at the
SUPERBLOCK partition symbol and its context, which is a scheduling
decision rather than another step inside this lane. Flagged as such.

**Still open, same structure family:** the scan-table disagreement recorded
above (unit 1418, values matching by scan index but positions not). It is
in this same block/transform-structure family and should be looked at
alongside this boundary.

**Hand-off, new class.** The new first divergence is at coefficient unit
2427 / TU 2428, mi(64,24), where the oracle codes a **32x32** luma TU and
we code **8x8** leaves. That is block/partition structure, not a neighbour
vote — a different class from this fix — so it is reported rather than
chased. `a1.obu` stays unpinned and no gate is written.

**On the scan-table finding (Main's caution): checked, and the caution does not
explain it.** Unit 1418 is mi(48,28) plane 2 and it PAIRED on both sides as 8x8
with an identical prediction sum (9024) — the tiling divergence (TX_4X4 vs
TX_8X8) is at unit 1419, the NEXT unit. So unit 1418 is 8x8 on both sides and a
"different scan table by design because the tx size differs" explanation does not
apply to it. The finding stands, though it may still be downstream of something
else; it is second priority to the vote.

### Unblock

The per-TU window bisect below has been run and NAMES the read: the **DC sign
context** at the sign pass of unit 1418, `dcctx` 1 (oracle) against 0 (ours),
taken in libaom from `txb_ctx->dc_sign_ctx` — a 3-symbol read from
`dc_sign_cdf_extra[plane_type]` per transform block. Next step: add a rung for
that symbol on BOTH sides (it is the one read in the unit that no
`EC_COEFF_STEP` print covers) and pair it, which should say outright whether our
value is wrong or merely drifted. The scan-table disagreement found in the same
window (values match by scan index, positions do not) is a second, independent
lead to keep open. Fixtures and dumps are in
`~/.cache/av1422warp/` (`a1_f0.obu`, `f0.opred`, `f0.mpred`, `f0.ocoeff`,
`f0.mcoeff`, `f0.ostep`, `f0.step`, `f0.otx`); the probe bypass
(`EC_AV1_ALLOW_422_PROBE`) is reverted and in no commit.

This is a 4:2:2-only entropy desync; no 4:2:0 or 4:4:4 evidence points at it, and
both controls stayed byte-exact throughout.

## Round 20 — EC_SYMR restored, tree protected, and the POISONER NAMED

### Restoration and protection

`EC_SYMR` had been lost on BOTH sides (my accidental `git checkout` of
`aom_dsp/bitreader.h`, and an earlier one of `msac.rs`). Rebuilt to skill
`ec-av1-consumption-gap-symr` Step 3 — ours an env-gated eprintln in
`SymbolDecoder::symbol` printing `pre=(value,range,bit) cdf0=<PRE-adapt
row[0]> n=<cdf.len()-1> s=<symbol> post_rng=<rng>`; the oracle the same
fields around `aom_read_cdf`/`update_cdf`, with value = dif top-16 and
bit = `8*(bptr-buf)-(cnt+15)`.

The validation bar also caught a SECOND defect I had introduced earlier:
11 format strings in the oracle's `decodetxb.c` carried `\\n` (a literal
backslash-n) where `\n` (the newline escape) belongs, collapsing each
unit's whole step run onto one line. Repaired. **Validation: the
`all_zero` chain on a1_f0.obu now byte-matches the saved oracle trace
(2512 lines) and 744 `name=mode` lines are emitted.**

The oracle tree is now snapshotted — `86c3958 oracle instrumentation
snapshot: EC_SYMR + rungs` — so a stray checkout cannot wipe it again.
Pre-edit copies of both files also sit in
`~/.cache/av1422warp/oracle-backup/`.

### The poisoner, named

`EC_SYMR` makes the two streams 1:1 BY CONSTRUCTION (both emit one line
per symbol read, in decode order), which is what the earlier
bit-position pairing could not do. On a1_f0.obu:

```
reads: oracle 46260, ours 46260
reads 0..45259 : MATCH on value, range, CDF row (32768 - ours == oracle),
                 alphabet, symbol AND post-read range
read  45260    : FIRST DIVERGENCE
    oracle pre=(27066, 42934, 92317) cdf0=15650 n=2 s=0 post_rng=45112
    ours   pre=(27066, 42934, 92332) cdf0=17486 n=2 s=0 post_rng=46114
                (as 32768- : 15282)
```

Same pre-state, same bit position, same alphabet, same symbol — a
**different CDF row**. The trace names the read:

```
oracle  EC_COEFF_STEP tag=sign c=0 sign=0 dcctx=0    <- row 0
ours    EC_COEFF_STEP tag=sign_rect pos=0 sign=0 dcctx=1  <- row 1
```

It is the **DC sign read of the chroma-U 4x4 transform of the 32x16 strip
at mi(68,16)**. The strip is decoded by `decode_block_rect`, which built
its chroma vote with `around_rect` and no 4:2:2 rerouting — the SIBLING
of `decode_leaf_rect`, which the round-6 fix gated.

### The fix (`d7c25c3c`)

`decode_block_rect` now routes planes 1 and 2 through
`around_mi_422_chroma` under the same `ss_x == 1 && ss_y == 0` gate the
square/leaf paths use. Luma keeps the per-mi gather; 4:2:0 and 4:4:4 keep
the plain rect walk verbatim.

**Red/green.** All 46260 reads now match at STATE level; the only
residual difference is the `bit` field over the last 17 reads, which is
the reference's own buffer-end tell convention (its source says the tell
offset "becomes important once we hit the end of the buffer"). Frame 0
is **pixel-exact against aomdec for the first time on this stream**; the
16-frame measure goes **0/16 -> 3/16** frames exact and **1619115 ->
1332125** differing samples.

**Identity, all four unchanged:** 4:2:0 control `a420.obu` byte-identical
to `aomdec --rawvideo`; 4:4:4 `444_sb128rect_lr_witness.obu`
byte-identical to the pre-lane build; pinned `422_allskip_2f.obu` and
`422_sb128_3f.obu` still pixel-exact.

### Not done

`a1.obu` is still not all-exact (3/16 frames), so it stays unpinned and
no gate is written. The next divergence is past frame 0 and was not
localized this round — the natural continuation is the same EC_SYMR
sequence diff run over the whole 16-frame stream, which now has a
trustworthy instrument and a per-TU exactness baseline.

## Round 21 — full-stream diff: frame 1 is a RECON defect, frame 2 is a NEW entropy class

Bypass reverted, worktree clean at `9c86a18f`, 4:2:0 control byte-exact.

### The EC_SYMR sequence diff over all 16 frames

```
oracle 246735 reads, ours 166211
reads 0..107516 : MATCH on value, range, CDF row, alphabet, symbol, post_rng
read  107517    : FIRST DIVERGENCE (frame 2)
    oracle pre=(45039, 61960, bit 3129) cdf0=24278 n=2 s=1 post_rng=45863
    ours   pre=(45039, 61960, bit 3144) cdf0=6401  n=8 s=3 post_rng=45500
                                               (as 32768- : 26367)
```

Identical coder state and identical bit position, but the two decoders
take a **different syntax branch**: the oracle reads `n=2`, then `n=2`,
then its `n=8`; this decoder reads its `n=8` straight away. That is a
MISSING read, not a wrong row — a different class from the two closed
defects (the OBMC chroma stride and the chroma dc-sign vote), both of
which were same-position/same-value/different-row.

Our block at that point is a sub-8x8 leaf, `EC_IMODE mi_row=24 mi_col=57
fn=sub8`, reached after its own `skip`/`cdef`/`dq` reads — all of which
pair. So the two decoders agree up to the block's mode-info and then
disagree about what the block IS.

### Two separate residuals, not one

Per-frame differing-sample counts against `aomdec --rawvideo`:

```
frame  0:      0   EXACT     frame  8: 118214
frame  1:  33375            frame  9: 103043
frame  2:  57364            frame 10: 119670
frame  3:  59542            frame 11: 126676
frame  4: 109837            frame 12: 127177
frame  5:  96361            frame 13: 133154
frame  6: 115869            frame 14: 131843
frame  7:      0   EXACT     frame 15:      0   EXACT
```

**3/16 frames exact (0, 7, 15)** — up from 0/16 — and the total is
1332125 differing samples, down from 1619115.

**RETRACTED — see round 22.** This section indexed pictures by SHOW
order while the dumps and the EC_SYMR sequence are DECODE order. Scanned
correctly there is ONE defect: picture 3 is the first damaged picture and
the read-107517 entropy divergence is in that same picture.

### Handoff

Two open items, neither in the closed family:

1. **Frame 1, reconstruction only.** Entropy exact, pixels wrong. Compare
   the stage dumps for frame 1 (PREFILT/POSTDEBLOCK/POSTCDEF) to localise
   it; the ladder will not help, it is already clean there.
2. **Frame 2 read 107517.** A missing read: our sub-8x8 leaf at
   mi(24,57) takes an `n=8` mode read where libaom takes `n=2`, `n=2`,
   `n=8`. The first step is to establish what the two `n=2` reads ARE —
   the natural candidates at 4:2:2 are the chroma-reference pair
   bookkeeping (`is_chroma_reference`: one chroma column per PAIR of luma
   mi columns) and a frame's intra/inter block decision. The
   sequence-numbered EC_SYMR diff is now a working instrument for that,
   with per-TU exactness on frames 0/7/15 as the baseline.

Not pinned, no gate, refusal untouched.

## Round 22 — the stage exoneration, and a CORRECTION to round 21

No code shipped this round; the tree is back at `401118ea`, bypass
reverted, 4:2:0 control byte-exact.

### CORRECTION to round 21: there is ONE defect, not two

Round 21 said "frame 1 is a reconstruction defect with exact entropy" and
"frame 2 is the new entropy class". That was **output-index confusion**:
the stage dumps are indexed by DECODE-order picture, the raw output by
SHOW order, and with alt-ref they differ. Scanned properly, over the
decode-order pictures:

```
pictures 0, 1, 2 : PREFILT identical AND final identical
picture  3       : PREFILT DIFFERS (first Y at (128,0), U at (128,1), V at (128,9))
pictures 4..15   : all differ
```

So picture 3 is the first damaged picture and the EC_SYMR divergence at
read 107517 is IN picture 3 — the same defect, entropy and pixels
together. The round-21 "two residuals" framing is withdrawn.

### Loop filters exonerated

On the oracle's own frame-1 dump, `post-deblock` and `post-CDEF` both
differ from `final` (first difference at index 116), so CDEF and loop
restoration are **active** on this picture, and the picture is exact at
PREFILT anyway. The defect is therefore upstream of the loop filters —
reconstruction, reached through the entropy desync, not a filter stage.

### The block, named precisely

At read 107517 both decoders are at the same coder state
(pre `value=45039, range=61960`, same bit). The block is the 4x4 sub-8x8
leaf at **mi(24,57)** and the syntax pairs to its mode:

```
ours    mi(24,57) skip cdef dq  mode -> 41536            ... and stops
oracle  mi(24,57) skip cdef dq  mode=0  angle_y=0
                   uv_mode=13  angle_uv=0  use_filter_intra=1  filter_intra_mode=0
```

**RETRACTED — see round 23.** "This decoder reads no chroma syntax at
all on that leaf" was read off `EC_ISTEP`, and
`read_intra_mode_sub8` emits no `istep!` for `uv_mode` or
`use_filter_intra`, so their absence proves nothing. The leaf IS reached
through `decode_leaf_split4` with `has_chroma=true`.

libaom's actual predicate (`av1_common_int.h:1459-1460`) is per axis

```
(mi_col & 1) || !(bw & 1) || !subsampling_x
```

so for a ONE-mi-wide leaf — what a VERT split of an 8x16 strip leaves —
the chroma reference is the ODD column, whichever leaf index that is.
`decode_leaf_split4` already implements exactly that (`(lmi.1 & 1) == 1`).
`decode_leaf_rect8`'s vert clause is `!vert || i == 1`, which names the
SECOND leaf unconditionally: right only when the split starts on an even
column, inverted when it starts on an odd one.

**Implemented, measured, and REVERTED.** With the `i == 1` clause
corrected to the column parity, the 16-frame EC_SYMR sequence diff is
**bit-identical to before** (first divergence still 107517) — this stream
never reaches the inverted case, so the change is correct by source and
**unexercised here**. Shipping an unproven change is not this lane's
practice, so it is recorded as a candidate rather than committed.

### Handoff

The defect is a chroma-reference test that is wrong on an odd mi column.
Two of the three sub-8x8 arms are now known: `decode_leaf_split4` is
correct, `decode_leaf_rect8` is correct-by-source but unexercised. **A
trace of `has_chroma` fired 268 times on this stream and never once for
mi(24,57)**, so the arm that reads that leaf has not been identified —
neither `decode_leaf_split4` nor `decode_leaf_rect8` is called for it.
The first concrete step is to find that third path (or the condition
under which those two are reached for a 4x4 leaf at an odd column),
apply the `av1_common_int.h:1459-1460` predicate, and re-run the
sequence diff: the EC_SYMR instrument is a working oracle for it and
pictures 0/1/2 are per-TU exact as the baseline.

Not pinned, no gate, refusal untouched.

## Round 23 — the third path IS `decode_leaf_split4`, and round 22's read of it was a rung gap

No code shipped; tree back at `7d274652`, bypass reverted, 4:2:0 control
byte-exact.

### Path identified (`#[track_caller]` on `read_intra_mode_sub8`)

The 4x4 sub-8x8 leaf at mi(24,57) is read by the call at
`decode.rs:21098` — inside **`decode_leaf_split4`**, not a third path.
The attribution of all 370 sub-8x8 mode reads splits 236 /
`decode_leaf_split4` and 134 / `decode_leaf_rect8`. The round-22
conclusion that "neither arm is called for it" was wrong: the
`has_chroma` trace added in round 22 was grepped on the wrong field name
(`col=` against a format that prints `lmi=(...)`), so its "never fired
for mi(24,57)" was a grep artefact.

Re-run correctly, at that leaf:

```
HC split4 lmi=(24,55) i=1 c422=true has_chroma=true
HC split4 lmi=(24,56) i=0 c422=true has_chroma=false
HC split4 lmi=(24,57) i=1 c422=true has_chroma=true
```

**So `has_chroma` is TRUE at mi(24,57)** and this decoder's chroma
gate is open and correct there.

### Round 22's central claim is WITHDRAWN

Round 22 concluded "this decoder reads no chroma syntax at that leaf,
where libaom reads uv_mode/angle_uv/use_filter_intra/filter_intra_mode"
and built the odd-column theory on it. That was read off `EC_ISTEP` —
and **`read_intra_mode_sub8` emits no `istep!` for `uv_mode` or
`use_filter_intra` at all** (its `chroma` and `filter_intra` blocks print
only under the separate `trace` flag, decode.rs:20406-20449). Their
absence from `EC_ISTEP` is a rung gap, not evidence that the symbol went
unread. This is the same class of error as rounds 8, 11 and 15: a
conclusion from a print without checking that the print covers the site.

What survives from round 22: the loop filters are exonerated, and the
first damaged picture is 3 with the read-107517 divergence in it.

### What the evidence now supports

`has_chroma=true` at mi(24,57) and yet the EC_SYMR sequence diverges
there — so if the chroma symbols are read (which the open gate says they
should be), the divergence is a VALUE or ROW difference inside them
rather than a missing read, and the "n=2, n=2, n=8 against a bare n=8"
shape needs re-reading against a trace that actually covers the sub-8x8
chroma symbols.

`decode_leaf_rect8`'s column-parity correction is therefore **still
unlanded** — the evidence that motivated it has collapsed, and it stays
correct-by-source-but-unwitnessed rather than committed on a theory
this lane has already had to retract once.

### Handoff

The right instrument, and it is a small edit: put the **mi coordinates
into the `EC_SYMR` print itself** (it currently has no mi, which is why
read 107517 could only be located by a fragile cross-file correlation
against a trace whose rungs do not cover the sub-8x8 chroma symbols).
With `EC_SYMR ... mi=(r,c)` on both sides, read 107517 names its own
block, the n=2/n=2/n=8 vs n=8 shape can be attributed to real symbols,
and the same diff runs per picture against the pictures 0/1/2 exact
baseline.

Not pinned, no gate, refusal untouched.

## Round 24 — the instrument works, and it MOVES the divergence to a different block

`c355c204` (instrument, no behaviour change; identity re-proved) on this
side, `35d97e5` in the oracle tree.

**`EC_SYMR` now carries `mi` on every read**, on both sides: ours a
thread-local the three mode readers set at entry, the oracle two globals
`ec_symr_mi_row/col` its mode-info reader publishes. Same-line fields.
Identity after the edit: 4:2:0 control byte-exact, 4:4:4 witness
byte-identical to the pre-lane build, both pinned 422 witnesses
pixel-exact.

`mi` is a LABEL for attribution and is deliberately **not** part of the
divergence criterion — the oracle publishes it one read later than we do,
so requiring equality reports a spurious divergence at read 5.

### It moves the answer

With the read naming its own block, the same divergence lands somewhere
**completely different** from rounds 22-23:

```
first DECODED-STATE divergence at seq=107517
  oracle mi=(64,56) pre=(val=45039, rng=61960, bit=3129) cdf0=24278 n=2 s=1 -> 45863
  ours   mi=(64,56) pre=(val=45039, rng=61960, bit=3144) cdf0=6401  n=8 s=3 -> 45500
                                                (as 32768- : 26367)
  [107513] n=2 s=0  | n=2 s=0     (pair)
  [107514] n=3 s=1  | n=3 s=1     (pair)
  [107515] n=3 s=0  | n=3 s=0     (pair)
  [107516] n=2 s=1  | n=2 s=1     (pair)
  [107517] n=2 s=1  | n=8 s=3     <-- DIVERGES
  [107518] n=2 s=1  | n=10 s=0
  [107519] n=8 s=3  | n=2 s=0
```

**The block is the 32x32 intra block at mi(64,56)** — the frame's
bottom-right corner at 4:2:2 (mi 64 of 72 rows, mi_col 56 of 64). Not the
4x4 leaf at mi(24,57) that rounds 22 and 23 named: that attribution came
from correlating the read stream against a SEPARATE trace, which is
precisely the failure mode the mi label removes. Rounds 22-23's
"the leaf reads no chroma syntax" and its odd-column theory are
**withdrawn twice over** and should not be revisited.

**The mode-info reads at mi(64,56) pair exactly** — skip 0, cdef 0, dq,
mode 3, angle_y -2, uv_mode 8, angle_uv -1, and both sides report the
same block size (ours `fn=sq side=32`, the oracle's `bsize=9` =
`BLOCK_32X32`). So the divergence is in the block's **COEFFICIENT** reads,
not its mode info and not its size: the oracle reads `n=2, n=2, n=8`
where this decoder reads `n=8, n=10, n=2` from the identical pre-state.
The `n=8`/`n=10` alphabets are transform-type sized, and at 4:2:2 this
block's chroma is a 16x32 rect, so the leading candidates are the
rect transform-type set (`av1_get_tx_type` over a rect plane block) and
an eob/tx_type ordering difference for the rect chroma unit.

### Handoff

The instrument is now trustworthy end to end: same-line fields, mi on
both sides, oracle tree snapshotted, and the divergence criterion
excludes the label. The next step is to attribute reads 107517-107519 to
symbols — with `EC_COEFF_STEP`'s `tag=` alongside (both sides emit it and
it now lines up read-for-read against the same mi) — and read the
transform-type set libaom selects for a 16x32 chroma plane block at
4:2:2 against the one this decoder selects.

Not pinned, no gate, refusal untouched.

## Round 25 — attribution blocked by a rung gap; a real underflow found on the way

New head for this section's code: `a4532a41`. Bypass reverted, worktree
clean, 4:2:0 control byte-exact.

### The attribution could not be completed — and why

`EC_COEFF_STEP` is emitted on both sides, but **neither side's tags
cover reads 107493-107517**: the last tagged read on both is 107492
(`sign`/`post_golomb`) and the next is 107542 on ours and 107752 on the
oracle's. So the `n=2,n=2,n=8` against `n=8,n=10,n=2` shape sits in a
region where no coefficient rung fires on either side, and the promised
lookup is not available without extending coverage into the phase that
reads there (the `n=2` pair is partition-alphabet sized and the
`n=8`/`n=10` are transform-type sized, which points at `read_var_tx_size`
in `decodeframe.c` — untagged on both sides).

### What the round did find: a real underflow in both OBMC walks

Chasing the tag gap surfaced a **debug-build panic on a1.obu**:
`attempt to subtract with overflow` at `decode.rs:31540`, then
`31491` — `overlappable_left` and `overlappable_above`, both called
from `decode_inter_block`.

The OBMC pair merge snaps back to the chroma pair's even row/column
(`row &= !1` / `col &= !1`, lane-obmcrec r1) so the ODD half is the
neighbour for both strips. That snap rounds DOWN, so at an ODD
`mi_row` / `mi_col` it lands on `mi_row - 1` / `mi_col - 1` and the
`row - mi_row` / `col - mi_col` offsets pushed into the neighbour list
underflow: `usize::MAX` in release, panic in debug. Both snaps are now
clamped with `.max(mi_row)` / `.max(mi_col)`; where the offset was
already non-negative the arithmetic is unchanged.

**Witness and its limit.** The panic reproduces on a1.obu with
`EC_TRACE_MODE_STEP=1`, single- AND multi-threaded, and does NOT
reproduce without that flag — which I could not explain, and am
recording as unexplained rather than inventing a reason. It is a
provable arithmetic defect with a panic as its witness, so it is fixed;
but it is **not** the frame-3 divergence: the 16-frame EC_SYMR
sequence diff is unchanged and the first divergence is still seq=107517
at mi(64,56). Identity re-proved after the edit: 4:2:0 control
byte-exact, 4:4:4 witness byte-identical to the pre-lane build, both
pinned 422 witnesses pixel-exact.

### Handoff

Two open items, and the first is now the smaller one:

1. **Extend the coefficient/transform-type rung into `read_var_tx_size`
   on both sides** (libaom `decodeframe.c`; this decoder's
   `read_var_tx_size` / txfm_partition read). That is what turns
   reads 107517-107519 from an alphabet guess into a symbol name, and
   it is the same shape as the two rung-coverage gaps this lane has
   already been bitten by.
2. **The mi(64,56) 32x32 block's coefficient reads.** With coverage in
   place, read the transform-type set libaom selects for this block's
   16x32 rect chroma plane block at 4:2:2 against ours. The `n=8` vs
   `n=10` alphabets already say the two sides are offering
   transform-type sets of DIFFERENT SIZES for the same block, which is
   a set-content or set-selection defect rather than a value one.

Not pinned, no gate, refusal untouched.

## Round 26 — coverage attempted, and it is NOT achieved: the gap is bigger than var-tx

Oracle rung added and snapshotted (`79e2986`): an `EC_VARTX` print on the
`txfm_partition` read at `decodeframe.c:1058`, carrying mi, row, col, ctx,
alphabet size, selected value and bit position. Validated at 1384 firings
on a1.obu. This decoder's `read_var_tx_size` already printed
`EC_ISTEP name=txfm_split`, so both sides now cover the var-tx phase.

**And the coverage check FAILS, which is the result.** Enumerating every
rung of any kind on both sides across reads 107495-107770:

```
OURS    (9 rungs)   first at 107533: EC_MM   mi_row=32 mi_col=0 w=64 h=64 ...
                    107537 txfm_split mi(32,0)  ctx=0
                    107539 txfm_split mi(32,8)  ctx=3
                    107762 EC_MM   mi_row=48 mi_col=0
ORACLE  (1 rung)    107751 EC_VARTX mi=(38,14) row=0 col=0 ctx=18 n=2 s=0 bitpos=3314
```

So reads **107493-107532 on ours and 107493-107751 on the oracle's are
untagged on BOTH sides** — the `n=8` (ours) and the `n=2, n=2` (the
oracle) that diverge at 107517 all sit inside that gap. It is not the
transform-type read: the var-tx rung fires 1384 times on the oracle's and
still nowhere near 107517. By the shape of the gap it is the
**inter-block mode-info and MV read path**, which neither side tags.

**The one thing this does establish about the divergence's direction:**
by read 107751 the oracle is at `mi(38,14)` while this decoder is at
`mi(32,0)`, and the oracle's bit position there is 3314 against our
3129 at the divergence — the oracle is roughly 230 reads and 185 bits
AHEAD. So this is not "one side reads an extra symbol"; the two decoders
are in **different blocks** by then, and the untagged gap is where they
part company.

**Not claimed:** any symbol name for 107517-107519, and any tx-set
comparison. Main's step 1 made coverage a precondition for step 2, and
the precondition is not met, so step 2 is not attempted.

**Handoff — the next instrument is the inter-mode/MV path, on both
sides**: tag the per-block inter mode-info reads (libaom
`read_inter_block_mode_info`: `is_inter`-dependent `y_mode`,
`uv_mode`, `skip_mode`, `interintra`, `comp_mode`, `motion_mode`) and
`read_mv_component`. With those, reads 107493-107532 stop being a gap
and the `n=8` / `n=2,n=2` get names; the `n=8` alphabet on our side is
the size a `y_mode` CDF would carry and the oracle's two `n=2` are
partition-sized, which is what makes "different block" the reading to
test first. Note the mi label will need the same treatment there — the
inter path's mi is not published by the intra mode reader.

Not pinned, no gate, refusal untouched.

## Round 27 — the inter path is tagged, and a wrong mi label of mine is fixed

Instrument `7f55d054` (phase + corrected inter mi, no behaviour change;
identity re-proved: 4:2:0 byte-exact, 4:4:4 byte-identical to the
pre-lane build, both pinned 422 witnesses pixel-exact) and oracle
snapshot `ec_symr_phase`.

### A label error of mine, found and fixed at the source

Round 27 first published the mi from `decode_inter_block`'s entry `at`.
That labels a **different block**: it reported mi(28,52) for reads this
decoder makes for mi(30,62). Our own `EC_MODE_VAL8` rung — printed by
the function that actually reads the mode — reports mi(30,62). The
phase/mi are now also published from `decode_inter_block8` at the
block's own `leaf_mi`.

This is round 22's misattribution class exactly, and it is now closed at
the source rather than worked around: with the label correct the two
sides are demonstrably in the SAME block.

### The divergence, correctly attributed

```
oracle  ph=inter   mi=(30,62) n=2  s=1     107517   <-- the two missing reads
        ph=inter   mi=(30,62) n=2  s=1     107518
        ph=inter   mi=(30,62) n=8  s=3     107519
ours    ph=inter8  mi=(30,62) n=8  s=3     107517   <-- identical to the oracle's 107519
        ph=inter8  mi=(30,62) n=10 s=0     107518
        ph=inter    mi=(32,0)  n=2  s=0    107519
```

Reads 107514-107516 pair at mi(30,62) on both sides. Then **this decoder
omits two 2-symbol inter-mode reads that libaom makes**, and its n=8 at
107519 is bit-identical to our n=8 at 107517 — a clean **two-symbol
shift, same block, same coder state** (pre `val=45039, rng=61960`).

**Not claimed: which two symbols.** libaom's 2-symbol reads in this
region are `interintra_cdf[bsize_group]` (decodemv.c:1628) and, after a
compound block, `comp_group_idx` / `compound_idx` (:1677, :1685); the
`n=3` pair just before (107514-107515) is in the MV/dmv region, which
this decoder's own path labels `inter8`. Pinning WHICH two requires
matching the read to its CDF context, and the `EC_SYMR` line carries the
`cdf0` pre-value but not the table identity — so the next step is to
extend the print with the CDF pointer (or the `bsize_group`) and pair on
that. I am not going to name them from alphabet size alone; that is the
same shortcut that produced rounds 8, 11 and 15.

### Handoff

Extend `EC_SYMR` on both sides with the CDF table IDENTITY (pointer or
`bsize_group`/`size_group` index) alongside `cdf0`. That is the last
thing `cdf0` alone cannot tell you — two rows can share a first entry —
and it converts "two 2-symbol reads" into the named pair. With that, the
gate that decides whether this decoder reads them is one comparison away
in the source.

Not pinned, no gate, refusal untouched.

## Round 28 — table identity added; the attribution is NOT closed

Instrument `05770240` (no behaviour change; identity re-proved: 4:2:0
byte-exact, 4:4:4 byte-identical to the pre-lane build, both pinned 422
witnesses pixel-exact) and oracle snapshot for the table names.

`cdf0` alone cannot identify a table — two rows can share a first entry
— which is exactly what stopped round 27. `EC_SYMR` now prints `cdf=`,
set at the read sites in this window on both sides: `interintra`,
`comp_group_idx`, `compound_idx`.

**What the oracle's tag says:** read 107517 at mi(30,62) carries
`cdf=interintra`, and the two reads this decoder omits sit either side
of it. The oracle's `interintra` value there is 1, so it then reads the
wedge follow-up as well.

**Why that is still not an attribution.** The matching gate on our side
(`enable_interintra_compound && !skip_mode`, decode.rs:41506) is **TRUE
on every one of its 451 firings** — `enable_interintra_compound=true,
skip_mode=false` — so this decoder is **not** skipping the interintra
read there. The obvious reading ("the two missing reads are interintra")
therefore does NOT hold, and I am not going to ship it. Two things are
unverified and both must be settled first:

1. **Our `cdf` tag has not been checked against a known read.** The
   printed field parsed oddly in the diff (it showed `inter8`, a value
   `set_symr_cdf` is never given), so either the tag is not being set on
   the path the reads are on, or my parse of the field is wrong. Until
   one known read is confirmed tagged correctly on our side, our half of
   the window is unlabelled.
2. **libaom's gate is a strict superset of ours** — `decodemv.c:1625` is
   `enable_interintra_compound && !skip_mode && is_interintra_allowed(mbmi)`
   and we implement the first two only. That makes libaom MORE restrictive, so
   it cannot explain libaom reading and us skipping; but it is a real
   divergence from source and should be reconciled on its own.

**Handoff.** Verify our `cdf` tag end-to-end on a read whose table is known
from an existing rung (the `EC_COEFF_STEP tag=tx_type` sites and our
`inter_txbset_for` are the natural anchors), then re-run the window. Only
with our side labelled is the two-symbol shift attributable; until then the
only sound statement is the one above — the oracle reads an `interintra`
symbol at mi(30,62) that this decoder does not read at that point, our
interintra gate is open there, and the two cannot both be true without a
label we have not verified.

Not pinned, no gate, refusal untouched.

## Round 29 — our tag was printing the PHASE; fixed, and the window is now labelled

`a4a1fcdb`: the `cdf=` slot of `EC_SYMR` was passing `SYMR_PHASE` a second
time — an off-by-one from adding the phase argument in round 27 and the cdf
one in round 28. **Every read reported the phase in the cdf field.** That is
precisely the `inter8` round 28 saw where no code path ever sets that value,
and it is why this lane could not label its own half of the window. With it
fixed the tag is live and verified against three distinct tables on a1.obu:
`interintra` 72533, `compound_idx` 33655, `comp_group_idx` 13744. No behaviour
change; identity re-proved (4:2:0 byte-exact, 4:4:4 byte-identical to
pre-lane, both pinned 422 witnesses pixel-exact).

### The window, both sides now labelled

```
  [107516] O cdf=interintra mi=(30,62) n=2 s=1 | M cdf=interintra mi=(30,62) n=2 s=1   pair
  [107517] O cdf=interintra mi=(30,62) n=2 s=1 | M cdf=interintra mi=(30,62) n=8 s=3   <-- DIVERGES
  [107518] O cdf=interintra mi=(30,62) n=2 s=1 | M cdf=interintra mi=(30,62) n=10 s=0
  [107519] O cdf=interintra mi=(30,62) n=8 s=3 | M cdf=interintra mi=(32,0)  n=2 s=0
```

Our `n=8` at 107517 is bit-identical to the oracle's at 107519, and the `n=10`
that follows matches too — so the oracle performs **two extra 2-symbol reads**
at mi(30,62) that this decoder does not, and the oracle's `interintra` value
there is **1**, so libaom then reads the wedge follow-up
(`decodemv.c:1639/1642`) as well.

### What is still NOT closed, precisely

Our `interintra` gate (`decode.rs:41506`, `enable_interintra_compound &&
!skip_mode`) is TRUE on all 451 of its firings and we do read `interintra`
(72533 tagged reads) — so this decoder is **not** skipping the symbol
globally. The two missing reads are therefore not "our gate is wrong"; our
block at mi(30,62) reaches the `interintra` read at a different point in the
sequence than libaom's does. The two reads are NAMED
(`interintra` + its wedge follow-up, by the oracle's tag and its value of 1)
but the reason our path differs at that block is not yet established, and I am
not shipping a gate change on an unestablished reason.

### Not done from this round's charter

Step 3 — reconciling the missing `is_interintra_allowed(mbmi)` term
(`decodemv.c:1625` is a strict superset of ours) — is **not** done. libaom's
extra term can only make libaom read FEWER interintra symbols, so it cannot be
the cause of the two extra reads observed here, and landing it blind would
change a gate on a stream I cannot yet explain. It stays open.

**Handoff.** With the tag live, the next step is to find which read this
decoder performs at mi(30,62) where the oracle performs `interintra` — i.e.
to tag the remaining inter-path reads (`y_mode`, `uv_mode`, `motion_mode`,
`skip`, and the `dmv`/MV reads) so the pair before 107517 is fully labelled.
Everything needed is now in place and verified.

Not pinned, no gate, refusal untouched.

## Round 30 — the tag WAS sticky; Main's suspicion confirmed, and round 28's attribution is retracted

`37656c2f` (instrument, no behaviour change; identity re-proved) and oracle
snapshot `662b6f0`.

### Sticky-tag verdict: CONFIRMED, and it invalidates round 28

The `cdf` tag was set at a read site and **carried by every later read**.
That is exactly how an 8-symbol read came to be labelled `interintra`, a
2-symbol table — the anomaly round 28 saw and could not explain. The tag is
now **one-shot**: `SymbolDecoder::symbol` consumes it, so an empty `cdf=`
means "no table set for this read", never the previous read's table.

**Consequence: round 28's "the two missing reads are interintra" is
RETRACTED.** It rested entirely on the sticky label.

### Remaining inter reads tagged, both sides

`y_mode`, `uv_mode`, `motion_mode`, `skip_mode`, `comp_group_idx`,
`compound_idx`, `interintra` (plus `mv_sign`, `mv_class` on the oracle).
Verified sparse, which is the check that matters: comp_group_idx 947,
interintra 945, uv_mode 895, motion_mode 736, compound_idx 726, y_mode 293,
everything else empty. A sticky tag would have inflated these.

### The window, with the label now trustworthy

```
  [107512] O -  n= 4 s=1 | M -  n= 4 s=1
  [107513] O -  n= 2 s=0 | M -  n= 2 s=0
  [107514] O -  n= 3 s=1 | M -  n= 3 s=1
  [107515] O -  n= 3 s=0 | M -  n= 3 s=0
  [107516] O -  n= 2 s=1 | M -  n= 2 s=1     pair
->[107517] O -  n= 2 s=1 | M -  n= 8 s=3     DIVERGES
  [107518] O -  n= 2 s=1 | M -  n=10 s=0
  [107519] O -  n= 8 s=3 | M -  n= 2 s=0
```

**What this now says.** The two extra 2-symbol reads are at sites that
carry NO table tag on either side — and the neighbouring `n=3` pair is
`mv_class` (MV_CLASSES is 3) with the `n=10` matching `class0_cdf`
(CLASS0_SIZE is 10). So the window is inside libaom's
**`read_mv_component`**, not the mode-info reads: the oracle reads two extra
2-symbol MV sub-reads per component that this decoder does not, and the
`n=8`/`n=10` pair that follows is the matching `y_mode`/`class0` read.

That is a real narrowing — from "two unnamed reads somewhere in the
inter path" to "two extra sub-reads inside the MV-component read" — and it
moves the defect OUT of the mode-info path that rounds 27-29 were chasing,
including the `interintra` gate theory, which is now irrelevant to this
window.

### Not closed

Which two MV sub-reads. `read_mv_component` (`decodemv.c:1000`) reads, in
order: `sign_cdf` (2), `classes_cdf` (MV_CLASSES), then under `class0`
`class0_cdf` (CLASS0_SIZE), else `class0_to_fr`/`hp`/`bits`. The two extra
n=2 reads are consistent with a `bit`/`hp`/`fr` sub-read this decoder skips
under a condition libaom does not take at this block — but naming WHICH
needs the sub-read sites tagged at their own granularity, not the
component.

**Handoff.** Tag `read_mv_component`'s sub-reads individually on both sides
(`mv_sign`, `mv_class`, `mv_class0`, `mv_fr`, `mv_hp`, `mv_bit`). With the
tag now one-shot, the two extra reads name themselves on the next pass.

Not pinned, no gate, refusal untouched.

## Round 31 — the two reads are NAMED: `decodeframe.c:1722`, `wiener_restore_cdf`

`37656c2f` (round 30's instrument, still current) and oracle snapshots
`662b6f0` / `9bb526a`. No behaviour change; identity re-proved.

### The instrument that finally worked

Two candidates were tried and **rejected on evidence**:
`__builtin_return_address(0)` is defeated by inlining and identical-code
folding — five distinct reads resolved to ONE address, and rebuilding
with `-fno-ipa-icf` did **not** separate them — and `addr2line` without
`-g` has no line info at all.

What works is a **macro capturing `__FILE__`/`__LINE__` at the expansion**,
which is exact by construction and survives both. `aom_read_symbol` now
reports `site=file:line` for every read. This is the instrument nine
rounds of pairing needed and did not have.

### The named reads

```
decodemv.c:1031  n=4   mv_fr
decodemv.c:1642  n=2   (interintra follow-up)
decodemv.c:172   n=4
decodemv.c:1654  n=2   wedge_interintra
decodemv.c:1232  n=3
decodemv.c:1232  n=3
decodeframe.c:1722 n=2 s=1     pairs
decodeframe.c:1722 n=2 s=1     <-- MISSING
decodeframe.c:1722 n=2 s=1     <-- MISSING
decodeframe.c:1268 n=8 / n=10  the next block's modes
```

`decodeframe.c:1722` is `wiener_restore_cdf` inside `read_restoration_type`
— the per-plane loop-restoration read-back. **The oracle reads it three
times at this superblock; this decoder reads it once.** That is the whole
divergence: this decoder is missing two loop-restoration unit reads.

### Two candidates checked and RULED OUT against libaom

1. **Chroma LR unit size.** `decodeframe.c:1584-1593` uses
   `s = AOMMIN(subsampling_x, subsampling_y)` and shifts by
   `aom_rb_read_bit(rb) * s` only when `s && !chroma_none`. At 4:2:2
   `s == 0`, so chroma takes the luma size unshifted — which is exactly
   what `ec-av1-syntax/src/frame.rs:1636` does. **Our code matches.** A
   plausible-looking "fix" here (`>> (subsampling_x + subsampling_y)`,
   the reading I started from) would break 4:2:0 and 4:4:4.
2. **`count_units`.** `restoration.rs:467` is character-for-character
   `av1_lr_count_units` (`restoration.c:63`): same half-up rounding, same
   `.max(1)`. **Matches.**

### What is left, and it is small

`restoration.rs:519-524` — the per-superblock unit range:

```rust
let mi_size = if plane == 0 { 4 } else { 4 >> ss_x(fctx) };
let rcol0 = ceil_div(mi_col * num_x, denom_x);
let rrow0 = ceil_div(mi_row * mi_size, unit_size);
```

`mi_size` is the only 4:2:2-sensitive term (2 at 4:2:2, 2 at 4:2:0, 4 at
4:4:4), and this range is what decides how many `read_lr_unit` calls a
superblock makes per plane. libaom's equivalent bounds come from
`av1_loop_restoration_corners_in_sb`, which mixes the superres `denom_x`
path differently for the row axis than this does. **That is the next
thing to check, and it is a read, not a rewrite.**

**Handoff:** compare `restoration.rs:519-524` line by line against
`av1_loop_restoration_corners_in_sb` (`restoration.c`), with the 4:2:2
`ss_y == 0` case in view. The oracle now names every read, so a correct
fix is provable in one run: the sequence diff must go flat.

Not pinned, no gate, refusal untouched.

## Round 32 — FIXED. The stream pairs bit-for-bit; the witness is pinned

`af3285d5`. The 4:2:2 header refusal is **unchanged and unconditional**.

### Round 31's hand-off was one of two errors, and they cancelled

Round 31 pointed at `restoration.rs:519-524`. It was right that the row
term was wrong, and it was **half** the story.

1. **`read_lr` derived the ROW corners from the COLUMN axis.** libaom's
   `av1_loop_restoration_corners_in_sb` (`restoration.c:1303-1338`) steps
   rows by `MI_SIZE >> subsampling_y` and columns by
   `MI_SIZE >> subsampling_x` — the axes are not symmetric. This decoder
   had one `mi_size = 4 >> subsampling_x` for both, on a base of 4 where
   libaom's `MI_SIZE` is 8. At 4:2:2 (`ss_x` 1, `ss_y` 0) the row range
   came out 4x too small, so `rrow0 == rrow1`: an empty range, zero
   `read_lr_unit` calls, wherever libaom reads a chroma unit. **Those were
   the two missing `wiener_restore_cdf` reads.**

2. **The superblock extent was 2x too large.** `read_sb128_root` passed
   `SB_MI * 2` for the 128x128 superblock, and the two 64px call sites
   carried the same half-mi convention (`sb_r * SB_MI`, extent `SB_MI`).

**`4 * 32 == 8 * 16`: the two errors cancelled exactly at 4:2:0.** That is
why 4:2:0 and 4:4:4 decoded byte-exact while carrying the bug, and why
the 4:2:0 evidence was never evidence of correctness here. At 4:2:2 the
cancellation breaks, because the row term stops following the column axis.
No amount of 4:2:0 testing could have found this.

A third, smaller correction: the `read_lr` call is the **one** consumer
that turns a superblock's mode-info origin into *absolute* frame space
(it divides by the restoration-unit size, a real pixel count), and it was
handed `(sb_r & !1) * SB_MI` — twice the true mi row, since `sb_r` indexes
64px superblocks. The partition decode is self-consistent with that value
(relative offsets only), which is why the 128 path decoded correctly
everywhere else. The origin is corrected **only** where it is converted
to absolute frame space; the partition path is untouched.

### Evidence

| check | result |
|---|---|
| entropy pairing vs instrumented oracle | **246735 reads each side, no divergence anywhere** |
| 16 frames vs `aomdec --rawvideo` | **pixel-exact**, 2359296 bytes, sha256 `4bfc2395…aeb203` both sides |
| 4:2:0 control | byte-exact |
| 4:4:4 LR witness | byte-identical to its pre-lane decode |
| both previously pinned 4:2:2 witnesses | pixel-exact |
| LR gate family | 12/12 |
| battery: 420/444/inter/superres/cdef | 118 passed, 0 failed, 7 ignored |

The pairing result is the strong one: not "past the window", but **no
divergence in the whole stream**.

After the first two edits, two committed 4:2:0/4:4:4 LR gates **failed**.
That is how the 64px call sites' half-mi convention surfaced; fixing them
is part of this change, not a follow-up.

### Pinned witness and gate

`crates/ec-av1/fixtures/422_residual_compound_warp_16f.obu` — 38845 bytes,
sha256 `d78e2afb43ce311d3d82335a945c6f80f62db4537966d881c1349a68b3aecb95`,
fnv1a64 `0x0e73a51e2cc0c424`. Gate
`the_pinned_422_residual_compound_warp_witness_is_present_and_refuses_by_name`.

The gate is **pin + refuse-by-name**, the established 4:2:2 pattern: with
the header refusal standing, no committed test *can* decode a 4:2:2
stream, because the bypass that would allow it must never be committed.
The exactness and engagement figures live in the gate's doc comment with
the encode recipe. Mutation-proven both ways: a flipped fixture byte
panics `bytes drifted`; a wrong refusal string panics `must refuse by
name`.

### Engagement, above the vertical midpoint

Every pre-existing warp counter is frame-global, so this adds
`top_half_warp_hits` and `top_half_compound_hits` (permanent gate
counters; the throwaway dump and its example wiring were removed):

- **top-half warp 248**, **top-half compound 3**
- frame-globally: compound_warp 25, compound_warp_8 18, rotzoom_gm_warp
  84, lr_wiener 22, lr_sgrproj 10, cdef_idx 53, part128_split 95

The loop-restoration numbers are load-bearing twice: decoding this stream
is what exposed the corner bug, and the fix is what made the sequence
pair.

### Refusal-lift assessment — exactness bar MET, coverage NOT yet

Not lifting, and here is the honest gap rather than a clean bill:

- 16 frames of **one** 256x288 stream from **one** encoder recipe;
- top-half **compound is 3 blocks** — thin, even though top-half warp is
  ample at 248;
- the code path whose bug we just fixed is *128x128 SB + loop restoration
  + non-4:2:0*. **4:2:2 with loop restoration OFF is still untested**,
  and it is a different range computation again (`av1_lr_count_units` over
  a full-height chroma plane with no units to place).

The old lift-blocker asked for a second 4:2:2 fixture with real residuals,
compound and warped motion above the vertical midpoint. This is that
fixture and it is exact. But one more exact stream is not the same as the
format being covered. **Recommendation: keep the refusal; charter the
LR-off 4:2:2 witness next**, then re-assess.

Not lifting. Refusal untouched. Nothing pushed.

## Round 33 — reviewer rework, five findings

All five accepted. Two were real defects the round-32 evidence had hidden, and
one of those two invalidates a number this report had already published.

### P2 (a4532a41) — the OBMC snap was clamped on the wrong value

`overlappable_above` / `overlappable_left` snapped the pair index with
`& !1` and then clamped the **snapped index** to `mi_col`/`mi_row` to stop the
reported offset underflowing. libaom (`obmc.h:44-46`) does the snap
unconditionally and reads at `snap + 1`; only the *reported* offset
(`above_mi_col - mi_col`) is negative, and it feeds
`av1_setup_build_prediction_by_above_pred` while the **read** feeds
`above_filter.get(mi_col + src4)` and the `MiInfo` itself. So the clamp
silenced the underflow and, in exchange, read the neighbour one column RIGHT
of libaom's and then stepped past `mi_col + 1` entirely.

Both sites now keep the snap unconditional and clamp only the offset
(`col.saturating_sub(mi_col)`, `row.saturating_sub(mi_row)`). Note the
representability point, because it is a real (if small) residual: the tuple is
`usize`, so libaom's `-1` is carried as `0`.

**New gate** `the_obmc_pair_merge_snap_reads_libaoms_neighbour_not_one_to_the_right`
pins the snapped READ with a synthetic grid carrying distinguishable
neighbours at the pair's even half, its odd half, and the column to the right.
Mutation-proven: restoring the old clamp on the column fails with
`left: (30, 30)`, restoring it on the row fails with `left: (30, 30) /
right: (20, 20)`. The test is necessary because **no stream in the corpus
exercises this branch** — a1.obu's 1338 traced OBMC neighbours are identical
before and after the fix, because its 4-wide pair never starts on an odd edge.

### P2 (msac.rs:448) — hot-path env lookup

`SymbolDecoder::symbol` called `std::env::var_os("EC_SYMR")` on **every symbol
read of every stream**, production formats included. Now
`crate::envflags::env_flag!("EC_SYMR")` (one `LazyLock<bool>` per call site, the
`ecdump_armed` precedent at msac.rs:1059). Proved byte-identical: the same
build with `var_os` and with `env_flag!` produces identical 246735-line
`EC_SYMR` traces.

### P3 — `MI_SIZE` doc (restoration.rs)

The constant's doc claimed "`MI_SIZE` (aom_scale.h): pixels per mode-info
unit". libaom's `MI_SIZE` is **4** (`av1/common/enums.h:39-40`,
`1 << MI_SIZE_LOG2`, `MI_SIZE_LOG2 2`) and lives in `enums.h`, not
`aom_scale.h`. The constant is renamed `MI_SIZE_8PX` and both its doc and
`read_lr`'s are restated in 8-pixel terms, with the reason the value is
double libaom's (its `denom` is a pixel count, so its numerator and
denominator already sit on opposite scales) and the paired trace named as the
authority. The rename exists so nobody "corrects" it back to 4.

### P3 (finding 5) — the top-half census was an upper bound, and 248 was wrong

`top_half_warp_hits` incremented on "either slot's global-motion model >
TRANSLATION" alone, omitting `is_globalmv && side >= 8`, so it counted blocks
that are not global-mv blocks. It now increments on `is_global_mv0 ||
is_global_mv1`, the same predicates libaom's `is_global_mv_block` uses, with
the census moved below their definition.

**The corrected upper-half figure is 3, not 248** (compound stays 3). The
frame-global numbers are unchanged: compound_warp 25, compound_warp_8 18,
rotzoom_gm_warp 84, lr_wiener 22, lr_sgrproj 10, cdef_idx 53, part128_split
95. The gate doc comment and the lift section are corrected; the earlier 248
was never a count of anything real.

Both accessors' `#[allow(dead_code)]` comments claimed they were "read only
from the `#[cfg(test)]` gates" / "read by the pinned 4:2:2 coverage gate".
**Neither has a committed reader and none can**, for the same reason the gate
cannot decode: the header refusal stands. Both docs now say that plainly.

### P2 (report headline)

Verdict, summary table, lift section and State rewritten to the final state:
16/16 pixel-exact, fixture pinned with its gate, refusal KEPT with the
LR-off / second-recipe reasoning. Per-round sections are left as history, so
the report still contains the superseded 248 and the earlier non-exact
verdict — they are dated by round, and the top of the file is the truth.

## Two refuted hypotheses — do not re-chase

1. **"libaom ORs the block's OWN `uv_mode` into the edge-filter type."** False.
   `reconintra.c:986` is `(above && is_smooth(above, plane)) || (left &&
   is_smooth(left, plane))` — neighbours only, no `mbmi->uv_mode` term.
2. **"the chroma smooth set is `uv_mode ∈ 7..=10`."** False. `enums.h:372-388` has
   no `UV_SMOOTH_135_PRED` at all (there is a `UV_D203_PRED` at 7), and
   `is_smooth` (`reconintra.c:958-970`) tests `{UV_SMOOTH_PRED,
   UV_SMOOTH_V_PRED, UV_SMOOTH_H_PRED}` = 9, 10, 11 — which is exactly what the
   committed `is_smooth_mode(9..=11)` already computes on the raw `uv_mode`
   symbol. The chroma numbering is libaom's `UV_PREDICTION_MODE` and it is NOT
   the luma `IntraMode` enum; the coincidence that makes the committed range
   right is that both enums number the smooth modes 9..=11.

Both were implemented, measured and reverted. Frame-0 PREFILT diff counts for
each variant of the smooth-type resolution: committed neighbours-only 56142;
own-mode OR'd in 62175; chroma range `8..=10` 63624; range `7..=10` 63624;
`7..=10` plus own mode 64759. All strictly worse than the committed form.

## Refusal-lift bar: NOT met — exactness is not coverage

The old blocker ("a second 4:2:2 fixture with real residuals, compound and
warped motion above the vertical midpoint would justify lifting") is now
**discharged as written**: that fixture exists, and it is exact. The refusal
still stays, on three grounds that a second exact stream does not answer.

1. **The top-half engagement is thin, and smaller than it first looked.**
   Measured with the census gated on libaom's actual `is_global_mv_block`
   (reviewer P3 corrected this — see below), the upper half carries **3**
   global-mv blocks and **3** `GLOBAL_GLOBALMV` compound blocks. An earlier
   figure of 248 came from a counter that tested only the global-motion model
   term and so counted every block with a non-`TRANSLATION` model on either
   slot, global-mv or not; it was an upper bound, not engagement. The
   frame-global figures are ample (25 compound-warp, 18 compound-warp 8x8
   leaf, 84 rotzoom global-warp) but they are not the charter's
   above-the-midpoint condition.
2. **One stream, one recipe.** Everything rests on a single 256x288 mandelbrot
   encode. A second independent encoder recipe would test the decoder against
   a different partitioning and motion-field structure rather than re-testing
   the same one.
3. **The path just fixed is still only half-covered.** What `af3285d5` repaired
   is *128x128 superblock + loop restoration + non-4:2:0*. **4:2:2 with loop
   restoration OFF** is untested, and it is a different range computation
   again (`av1_lr_count_units` over a full-height chroma plane with no units to
   place). A refusal lifted on the strength of the stream that found the bug
   would be lifting on the stream's own blind spot.

**Recommendation: keep the refusal; charter the LR-off 4:2:2 witness next,
then a second encoder recipe, then re-assess.** The lift decision should not be
made in the same round that fixed the bug the stream was built to find.

## Known unresolved

- **Left chroma reference row term (`+ss_y`, libaom `av1_common_int.h:1400-1401`)
  is NOT shipped.** `smooth_uv_neighbour_unsnapped` reads the left neighbour on
  its own mi row at every subsampling; libaom reads it one row down at 4:2:0 and
  4:4:0. Unblock: a stream with a 1-mi-tall left neighbour (an 8x4 leaf's left
  block) at 4:2:0/4:4:0 — no committed fixture has one, which is why the
  current read is byte-exact on the control and why the correct term cannot be
  witnessed today. The above column term (`+ss_x`) IS shipped, at every
  subsampling where `ss_x == 1`.

## State

44 commits on `lane-av1422warp`, no push. Worktree clean; the
`EC_AV1_ALLOW_422_PROBE` bypass is reverted and in no commit. All temporary
instrumentation was removed except what is documented above as a permanent
gate counter or as the `EC_SYMR` trace.

Final state, for the VPS suite:

- **pixel-exact**: 16/16 frames vs `aomdec --rawvideo`; entropy pairs
  246735-for-246735 with no divergence anywhere in the stream.
- **identity**: 4:2:0 control byte-exact, 4:4:4 LR witness byte-identical to
  its pre-lane decode, both previously pinned 4:2:2 witnesses pixel-exact, LR
  gate family 12/12, OBMC family 9/9, scoped battery (420/444/inter/superres/
  cdef) 118 passed / 0 failed / 7 ignored.
- **pinned**: `422_residual_compound_warp_16f.obu` with its gate.
- **refusal**: UNCHANGED and unconditional. The bypass is not committed.

Known unresolved, carried forward: the left chroma reference row term
(`+ss_y`, libaom `av1_common_int.h:1400-1401`) is still not shipped, for the
reason given above.
