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

## Defect 3 — NOT fixed, HANDOFF with paired dumps

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

### Unblock

The bit-position bisect below has been run and narrows this to a **6-bit**
excess consumed between unit 1418's and unit 1419's `txb_skip` reads. Continue
by restricting the per-coefficient step pairing to a SINGLE transform unit at a
time — the whole-stream pairing does not work, because the two readers traverse
a unit's coefficients in different orders. The 4:2:2-only constructs in that
window remain the candidates: the chroma-reference bookkeeping for the shared
mi(50,27) column, and whatever the 4:2:2 block tail reads after the shared
chroma. Fixtures and dumps are in
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
