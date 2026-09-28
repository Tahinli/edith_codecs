# lane-av1422warp — second 4:2:2 coverage witness: one crash fixed, one edge-filter reference fixed, one transform-size defect localized

Base `a7d22aec`, worktree `~/.cache/wt/av1422warp`, branch `lane-av1422warp`, no
push. Target dir `$HOME/.cache/cargo-target-av1422warp`.

## Verdict

The charter asked for a pixel-exact 4:2:2 stream with real residuals, compound
and warped motion above the vertical midpoint. **The stream is not pixel-exact**,
so it is NOT pinned and no coverage gate was written — a gate over a non-exact
stream is a false claim. What it bought is two real 4:2:2 defects fixed (one a
decode-blocking panic) and a third localized to one site for its owner. The
sequence-header refusal **stays**.

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
`set_mi_row_col` (`av1_common_int.h:1395-1412`) puts them at
`base_mi[-stride + ss_x]` and `base_mi[ss_y*stride - 1]`, where `base_mi` is the
block's own mi already de-offset by `(mi_row & ss_y)` / `(mi_col & ss_x)` — which
is exactly the snap the caller applies. So above is `(mi_r - 1, mi_c + ss_x)` and
left is `(mi_r, mi_c - 1)`: **`ss_y*stride - 1` is a COLUMN offset of -1** (its
row delta is 0 at every subsampling, since the row term is at most `stride - 1`
elements). Only the above COLUMN gains `ss_x`. One chroma column spans two luma mi
columns, so the chroma edge reaches into the right-hand luma block — that is
where its reference lives.

### How the split was chosen, and why 4:2:0 stays exact

An earlier attempt applied libaom's formula to BOTH reads (`above` row
`mi_r + ss_y - 1`, `left` row `mi_r + ss_y - 1`). That improved 4:2:2 (56142 →
53395) but regressed the byte-exact 4:2:0 control, and gating only the column
shift to 4:2:2 still regressed it — the row term, not the column term, was the
wrong part. Doing the offset arithmetic settled it: `ss_y*stride - 1` elements
from `base_mi` has row delta 0 for `ss_y ∈ {0,1}`, so the left read never moved.
The shipped form is libaom's own at every subsampling.

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

## Defect 3 — NOT fixed, for its owning lane

After `a9611ea2` the first 1419 TUs of frame 0 pair exactly on prediction sum;
index 1419 is the first divergence and it is a shape difference, not a value one:

```
1418 O (48,28,plane 2,'8x8', 9024)  M ('8x8', 9024)   <- matches
1419 O (50,28,plane 0,'8x8', 5715)  M ('4x4', 1424)   <- oracle codes ONE TX_8X8
1420 O (50,28,plane 1,'4x8', 3370)  M ('4x4', 1344)   <- luma TU first, chroma follows
1421 O (50,28,plane 2,'4x8', 2513)  M ('4x4', 1488)
```

Class: 4:2:2 transform-size selection for a luma block whose plane block is twice
as tall — the luma max transform at mi(50,28) is TX_8X8 for the oracle and TX_4X4
for us, and the chroma units follow. This is the family
`lanes/av1422bigblock.report.md` already fingerprinted as "sub-8/odd-strip 4:2:2
chroma under the 128-rect intra path (the `ss_size_lookup` BLOCK_INVALID
family)" — a known class with a known owner, not a new one. Unblock: one look at
the 4:2:2 `max_txsize_rect_lookup` / chroma-tx derivation for an 8x8 luma block
whose plane block is 4x8.

Our decoder also emits 3450 `OUR_PRED` lines where the oracle emits 2662
`EC_PREDOUT8` — same per-block unit-count family, worth the same lane's look.

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

## State

Two commits on `lane-av1422warp`, no push. Worktree clean; the
`EC_AV1_ALLOW_422_PROBE` bypass is reverted and in neither commit. All
instrumentation (`EC_FULLDUMP`, `EC_FDSTR`, the `OOB_BLEND_*` bounds rungs) was
removed before the commits. No suite run on this branch — Main's job.
