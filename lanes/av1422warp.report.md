# lane-av1422warp — second 4:2:2 coverage witness: one crash fixed, one edge-filter reference fixed, one transform-size defect localized

Base `a7d22aec`, worktree `~/.cache/wt/av1422warp`, branch `lane-av1422warp`, no
push. Target dir `$HOME/.cache/cargo-target-av1422warp`.

## Verdict

The charter asked for a pixel-exact 4:2:2 stream with real residuals, compound
and warped motion above the vertical midpoint. **The stream is not pixel-exact**,
so it is NOT pinned and no coverage gate was written — a gate over a non-exact
stream is a false claim. What it bought is two real 4:2:2 defects fixed (one a
decode-blocking panic) and a third localized to a single desync window and handed
off with paired dumps. The sequence-header refusal **stays**.

| commit | defect | before | after |
|---|---|---|---|
| `4579d209` | OBMC strides the 4:2:2 chroma prediction with the square `chroma_side` | 0 frames decoded (panic) | 16/16 decode |
| `a9611ea2` | chroma-above neighbour read one mi column left of libaom's | frame 0 PREFILT 56142 diffs | 53395 diffs; first divergent block byte-exact |

## The stream (throwaway, `~/.cache/av1422warp/`, not committed)

- `a1.obu` 38845 B, sha256 `d78e2afb43ce311d3d82335a945c6f80f62db4537966d881c1349a68b3aecb95`
- source `src422.y4m` sha256 `4d35eaf65d1a5541b3177e1183644c163b3868d8f141bed0ce9fdf833280ba9f`
- 4:2:0 control `a420.obu` sha256 `1f43ef1ace6f7fa2f38c2deff765dd0bc529cbf7432788c736c7261350525e5b`

```
ffmpeg -f lavfi -i "mandelbrot=size=256x288:rate=24:maxiter=220:start_scale=3:end_scale=0.35:end_pts=300,rotate=a=0.10*t:c=none:ow=256:oh=288" \
       -frames:v 24 -pix_fmt yuv422p -f yuv4mpegpipe src422.y4m
aomenc --codec=av1 --profile=2 --input-bit-depth=8 --limit=16 --width=256 --height=288 \
       --lag-in-frames=25 --auto-alt-ref=1 --enable-global-motion=1 --cq-level=24 \
       --cpu-used=0 --threads=4 --kf-min-dist=0 --kf-max-dist=99999 -o a1.webm src422.y4m
ffmpeg -i a1.webm -c:v copy -f obu a1.obu
```

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

## Refusal-lift bar: NOT met

Beyond the stream still being non-exact, the charter's other precondition is
unproven: compound and warped motion above the vertical midpoint has not been
measured on a pixel-exact 4:2:2 stream, because there is not one. The stream
does open the OBMC-chroma and chroma-edge-filter defects, which no prior 4:2:2
fixture reached — that is real coverage progress, and it is not the same thing as
the bar.

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

Two commits on `lane-av1422warp`, no push. Worktree clean; the
`EC_AV1_ALLOW_422_PROBE` bypass is reverted and in neither commit. All
instrumentation (`EC_FULLDUMP`, `EC_FDSTR`, the `OOB_BLEND_*` bounds rungs) was
removed before the commits. No suite run on this branch — Main's job.
