# lane-av1dcunit — the assigned defect was already fixed; the 2x1/1x2 class cell was not

Worktree `~/.cache/wt/av1dcunit`, branch `lane-av1dcunit`, base `70ed5601`
("Merge lane-av1txctxband"). Two commits: one read-only verification of the
assigned defect, one fix for the class-sweep cell the verification found.

## 1. Headline: the assigned defect is NOT on `70ed5601` — it was fixed 55 minutes before this lane started

`lanes/av1chromarect-r2.report.md` (the ticket's source) measured its 79969
samples on base `5324a685`. The fix landed on main **after** that base and
**before** the r2 report was even merged:

| sha | time | what |
| --- | --- | --- |
| `5324a685` | 01:36 | r2's base — 79969 chroma samples wrong |
| `57834ee2` | 01:49 | r2's report merged (numbers measured on `5324a685`) |
| `e05a6ceb` | **02:04** | **the fix**: "4:4:4 2x2 chroma walk: window the intra-BC/palette override per unit" |
| `2c0fd2b9` | 02:12 | merge of `lane-av1chromadc` — 79969 → 0 |
| `70ed5601` | 02:58 | this lane's base |

`git merge-base --is-ancestor e05a6ceb 5324a685` → false. The r2 report and the
fix were two lanes working the same residual from two bases; the fix won by 15
minutes. So there was nothing to fix for the ticket as written, and this lane
verified that rather than re-doing it.

## 2. The assigned defect, re-measured here (red-before included)

**Mechanism, and it is NOT the one the ticket names.** The ticket (following
r2 §3) says libaom "builds the DC from THAT UNIT's own above row and left
column (`av1_build_dc_predictor_sb`)". For this witness that is wrong, and the
seed proves it: the seed is `skip=1` on an **intra-BC** block, and an intra-BC
block never reaches `av1_predict_intra_block` at all. libaom's decoder routes
it through the inter path instead:

* `decodeframe.c:1146` — `int inter_block_tx = is_inter_block(mbmi) || is_intrabc_block(mbmi);`
* `decodeframe.c:847` `predict_inter_block` → `decodeframe.c:676` `dec_build_inter_predictor`
* `decodeframe.c:686-688` — one `dec_build_inter_predictors` call per plane with
  `bw = xd->plane[plane].width, bh = xd->plane[plane].height`, i.e. the WHOLE
  chroma plane block at once, not per transform unit
* `reconinter_template.inc:228` — `const struct buf_2d *const pre_buf = is_intrabc ? dst_buf : &pd->pre[ref];`
  the intra-BC reference **is the destination buffer**, so the prediction is a
  motion-compensated copy of the current frame's own reconstruction
* the units are then reconstructed in place on top of it, with no edge read

So there is no per-unit DC and no per-unit above/left on this path. What the
crate's whole-plane-block `PALETTE_PRED` override buffer is, in libaom's terms,
is that single whole-plane-block motion prediction — and
`PlaneBuf::reconstruct` consumes the buffer it is handed as the **unit's own**
`chroma_tx x chroma_tx_h` prediction (`decode.rs:20939` and `21087`,
`let prediction = if let Some(buf) = pred { buf } else { … }`). Handing the whole
buffer to every unit of a 2x2 grid makes all four units predict from the
top-left one. That is the defect, and `e05a6ceb`'s `chroma_window` is the
correct fix for it.

**Red-before, measured in THIS worktree** by mutating `chroma_window` back to
the pre-fix whole-buffer shape (that is mutation M1 of §5) and running the
landed gate `a_444_skipped_64x64_square_block_windows_its_chroma_override_per_unit`:

```
decode-order frame 0 plane U is byte-exact nowhere -- first divergence at
sample 229728 of 262144 (x=352, y=448), 7488 samples wrong.
test result: FAILED. 0 passed; 1 failed
```

Full per-plane counts, from my own compare script outside the repo
(`~/.cache/cmp444.py`, reading the `EC_AV1_FINAL_DUMP` dumps of both sides):

| frame | Y | U | V |
| --- | --- | --- | --- |
| 0 | 0 | **7488** | **8128** |
| 1 | 0 | **22571** | **41782** |
| 2 | 0 | 0 | 0 |

Total **79969 of 2359296** — r2's number exactly. The three regions the ticket
names, frame 0:

| region | U wrong | V wrong | first wrong |
| --- | --- | --- | --- |
| seed `[320,384)x[448,512)` | 864 / 2048 | 1024 / 2048 | (352,448) |
| sibling `[384,448)x[448,512)` | 1664 / 4096 | 1984 / 4096 | (416,448) |
| victim `[448,512)x[448,512)` | **4096 / 4096** | **4096 / 4096** | (448,448) |

f0 U, row y=448, every 16th sample from x=320 (the 2x2 grid boundary is
x = 352 = 320+32):

```
oracle: 241 239 202 202 202 202 203 165 166 166 166 166
ours  : 241 239 241 239 202 202 202 202 175 175 171 171
```

The seed's `cu=(1,0)` is a flat 241 — its sibling's value — exactly as the
ticket describes, and the victim is wrong on every sample.

**After (fix in place, same oracle, same script): 0 wrong on all three planes
of all three frames, 0 of 2359296.** All three named regions are 0/2048 and
0/4096. The f0 U row above matches the oracle in all 12 samples.

One correction to the ticket's "clean sibling control": `[384,448)x[448,512)`
is **not** clean under this defect — 1664 U and 1984 V samples wrong, first at
(416,448). What r2 measured clean was that block's *edge samples* are all ≈202,
so "any DC rule reproduces the oracle" for it (r2 §3); its pixels still move
because an intra-BC block's reference is the current frame's own
reconstruction, so the seed's error propagates one block to the right. The
first dirty cell is x=416, not x=352, for that block.

## 3. Acceptance

1. **Red-before per plane** — §2, 7488 / 8128 / 22571 / 41782, total 79969,
   dumped with `EC_AV1_FINAL_DUMP` and compared by `~/.cache/cmp444.py`.
2. **After** — 0 wrong on Y, U, V of all three frames; all three named regions
   exact. Nothing remains in the regions the seed/victim cover.
3. **Gate** — `a_444_skipped_64x64_square_block_windows_its_chroma_override_per_unit`
   (landed by `e05a6ceb`, aomdec oracle required, no silent skip). Pin
   `fixtures/r512.obu`, 6948 B, sha256
   `f587f0f980f149abce905e3ef66f2f1634f050c85430626eafb9c30a1e25bc21`,
   already committed — no regeneration needed.
4. **Mutation** — M1 below.
5. **Re-quotes, unchanged** — §4.
6. **Class sweep** — §5, and it found a real second defect, which this lane
   fixed.

## 4. Re-quotes on this branch (nothing in §2's path changed)

```
$ cargo check -p ec-av1 --all-targets        # 0 errors, 0 warnings
$ cargo test -p ec-av1 --lib -- a_444 lossless_444 intrabc_rect 444_lossless 420_intrabc_rect4
test result: ok. 39 passed; 0 failed; 0 ignored; 0 measured; 736 filtered out; finished in 12.44s
```

The 39 carry the 4:4:4-lossless family (`a_lossless_444_*`,
`a_444_lossless_sb64_intrabc_rect_chroma_walks_4x4_units`,
`a_real_aomenc_lossless_444_key_frame_decodes_sample_exact`), the intrabc_rect
family (`a_444_intrabc_owned_rect_*`, `a_444_intrabc_rect4_*`), and the 4:2:0
twin (`a_444_intrabc_rect_chroma_plane_block_is_the_block_footprint`, which
runs its `420_intrabc_rect4_witness.obu` sibling through
`decode_all_frames_vs_oracle`).

Wider family, once per tree, same filter string, same env, private
`CARGO_TARGET_DIR`:

```
$ cargo test -p ec-av1 --lib -- rect intrabc ibc chroma 444 420 subsampl
base 70ed5601 (clean, detached)       ok. 145 passed; 0 failed; 5 ignored; 0 measured; 623 filtered out; finished in 272.43s
this branch tip                       ok. 147 passed; 0 failed; 5 ignored; 0 measured; 623 filtered out; finished in 273.28s
```

145 -> 147: the delta is exactly this lane's two new gates and nothing else
(`--list` reads 775 tests on both trees). Same 5 `#[ignore]`d in both. **No
test was renamed, dropped or `#[ignore]`d.** **No floor was lowered anywhere**; the only new asserts are the two
gates of §5.

## 5. Class sweep — and the 2x1 / 1x2 cell was broken

Every site in the crate that walks a 4:4:4 chroma plane block as a grid of
`Chroma32` units, and what it hands each unit:

| # | site | grid reachable at 4:4:4 | per-unit **window** | per-unit **reach** |
| --- | --- | --- | --- | --- |
| 1 | `decode_block` skip arm (decode.rs:21897) | 2x2 (NxN) | `chroma_window` — fixed by `e05a6ceb` | `tu_reach_rect` ✓ |
| 2 | `decode_block` coded arm (decode.rs:22182) | 2x2 (NxN) | `palette_window` ✓ | `tu_reach_rect` ✓ |
| 3 | `decode_rect_split` `chroma_tiled` (decode.rs:12721) | **2x1, 1x2, and 2x2** | `palette_window` ✓ | **block `reach` — WRONG** |

Site 3 is the only one that can produce a **2x1 or 1x2** grid at all, and it
was the only one passing the block's `reach` to every unit. Sites 1 and 2 can
never be 2x1/1x2: a square luma block's chroma plane block has
`chroma_side == chroma_height` at every supported subsampling (4:2:0 → `>>1,>>1`;
4:4:4 → identity; 4:2:2 is the only asymmetric one and is refused by name at
the sequence header, `stream.rs:1783`), and `chroma_tx == chroma_tx_h`, so
`cn_cols == cn_rows` always. A 2x1/1x2 needs a rect luma block, i.e. site 3.

**The defect, measured.** `decode_rect_split`'s `chroma_tiled` walk passed the
block-level `reach` to every unit. libaom computes `has_top_right` PER
TRANSFORM UNIT — `av1_build_intra_predictors` calls it with the chroma txb
walk's own `blk_col`/`blk_row` (defined `reconintra.c:196`, called from
`av1_build_intra_predictors` at `reconintra.c:1847`), and the `row_off == 0`
arm is `col_off + top_right_count_unit < plane_bw_unit`, so the FIRST unit of a
2-wide grid reads its above-right out of the already-reconstructed block ABOVE
even when the block's own right edge is the frame's. `PlaneBuf::edges`
(`decode.rs:20840`) truncates the above row at `own_across` when
`above_right` is false, so a directional unit whose angle is below 90
(`need_right`, `intra.rs:546`) predicted from a 32-sample above row where libaom
used 64.

Witness encoded here (recipe and sha256 in the gate's doc comment):
`fixtures/r512_rect2x1_1x2_444.obu`, 2697 B, sha256
`a009ec580a1ff24250be53e11b8d09616da71e095cefd5eb621c941d40a8688b` — 4:4:4 8-bit
256x256, one key frame, and it reaches BOTH shapes: a 64x32 chroma plane block
as `nw=2 nh=1` of 32x32 and a 32x64 one as `nw=1 nh=2`.

Red-before, per plane, my own script:

```
f0: Y 0 wrong   U 130 wrong   V 30 wrong   (of 65536 per plane)
```

All 160 wrong samples are in the right 6 columns of the `cu=(0,0)` unit at
`mi=(48,48)` `px=(192,192)` — a 64x32 block whose right edge IS the frame's.
Oracle `EC_PRED` for that unit vs ours:

```
oracle  EC_PRED      mi_row=48 mi_col=48 plane=1 row_off=0 col_off=0 txw=32 txh=32
                      mode=1 p_angle=84 have_top=1 have_left=1 n_top=32 n_left=32
                      n_tr=32 n_bl=-1 bsize=11 part=1 ft=1
oracle  EC_PREDOUT8  ... sum=172662 row0=179,185,190,195,197,198,197,194 col0=179,180,181,181,182,182,183,183
ours    OUR_PRED     x=192 y=192 plane=1 side=32 mode=1 ad=-2 ft=1
                      sum=171512 row0=[179,185,190,195,197,198,197,194] col0=[179,180,181,181,182,182,183,183]
```

`bsize=11` is `BLOCK_64X32`; `n_tr=32` against our `above_right = false` is the
whole defect in one number. `row0` and `col0` match exactly and only the SUM
differs (172662 vs 171512, and 125978 vs 126008 on V), so the divergence is
prediction-side, not entropy. Stage-attributed against the oracle's own rungs:
the same 130/30 wrong appear in `EC_AV1_PREFILT_DUMP` and in
`EC_AV1_FINAL_DUMP` — pre-loop-filter, so not deblock/CDEF/LR. `n_tr=32` also
explains the shape of the error: the U delta per column is
-1, -5, -9, -14, -18, -19, growing monotonically rightward, which is a
directional prediction reaching further right as it goes.

**The fix.** One expression, both unit readers in the walk (the square
`read_plane` and the rect `push_intra_rect`), each given the unit's own reach:

```rust
let cu_reach = tu_reach_rect(
    bw, bh,
    (cu_col * uw) << ss_x(fctx),
    (cu_row * uh) << ss_y(fctx),
    uw << ss_x(fctx), uh << ss_y(fctx),
    reach, px, py, y.width, y.height, fctx,
);
```

`tu_reach_rect` is the crate's existing port of the per-unit rule (it is what
sites 1 and 2 already call), so this is site 3 joining the other two, not a new
rule. `cu_reach` is also what `of_tu` needs at 4:4:4: the ss shifts are
identity there and the per-axis spelling is what makes the same call correct at
4:2:0 and at 4:2:2 if 4:2:2 is ever ported.

**After: 0 wrong on all three planes.** Every region the sweep covers.

**Gates** (both aomdec-oracle, no silent skip, both with the counters asserted
before a single sample is compared):

* `a_444_rect_strip_chroma_tiled_2x1_and_1x2_units_take_their_own_above_right_reach`
  — asserts `>= 4` units in a 2x1 grid AND `>= 4` in a 1x2 grid, and
  `>= 1` unit whose per-unit reach differs from the block's, then full
  byte-exactness of all three planes of frame 0. Pin
  `fixtures/r512_rect2x1_1x2_444.obu`, fnv1a64 `0x2751ccaa079cc422`, sha256
  `a009ec58…`, recipe in the doc comment.
* `a_444_rect_strip_chroma_tiled_1x2_palette_grid_decodes_byte_exact`
  — the **UV-palette cell that `lanes/av1chromadc.report.md` §5 left explicitly
  unmeasured** ("every multi-unit walk on this witness is `src=intrabc`; the
  UV-palette source never reaches a multi-unit chroma walk here, so its arm of
  the sweep is argued from the shared code path, not witnessed"). Pin
  `fixtures/r512_rect1x2_444_palette.obu`, fnv1a64 `0x7759753a0d3f75a2`,
  sha256 `fad03c69…` — it puts a UV-palette block on a 1x2 16x64 chroma grid at
  `mi=(32,12)` (measured with a throwaway probe, since removed) as well as
  non-palette ones.

**Mutation proofs, each reverted:**

| # | mutation | result |
| --- | --- | --- |
| M1 | both unit readers back to the block `reach` (the pre-fix shape) | **RED** — `frame 0 plane U … first divergence at sample 49374 (x=222, y=192), 130 samples wrong` |
| M2 | `cu_col`/`cu_row` transposed in the `tu_reach_rect` offsets | **GREEN — and this is a real limit of the gate, not a pass** |

M2 is worth stating plainly. On this witness the second/bottom unit of each
grid sits flush against the frame's right/bottom edge, so
`Reach`'s `.min(self.width)` clamp in `edges`/`edges_rect` truncates the extra
reach back to nothing and the transposition becomes unobservable. The
transposed expression is still wrong, and no witness in this lane distinguishes
it. A gate that claimed to cover it would be a false claim, so neither gate
does: they pin the reach VALUE (M1 bites), not the offset spelling.

## 6. The other multi-unit `set_palette_pred` sites, for the record

All 19 `set_palette_pred` call sites were enumerated and each classified by its
enclosing loop. Every one is either already windowed per unit
(`palette_window` at decode.rs:12633, 12784, 16685, 16955; `chroma_window` at
21953/21975) or is a genuinely single-unit read where the whole buffer IS the
unit's window (`decode_block_rect` 15378/15396/15559/15607 and
`decode_leaf_rect` 15996/16014/16175/16220 read `chroma_w x chroma_h` once per
plane; `decode_block_rect4` 16825/16859 loops over PLANES of a 4x4 block, not
units). One site is unwindowed inside a unit loop —
`decode_rect_split` 12525, the `(4,8)` chroma units of a 4:2:2 `BLOCK_INVALID`
plane — and it is unreachable: `chroma_422_oob` and `chroma_422_pair_wide` both
require `ss_x == 1 && ss_y == 0`, i.e. the 4:2:2 that `stream.rs:1783` refuses
by name.

## 7. Not done

1. **The 2x1/1x2 offset spelling is unproven** (M2, §5). Closing it needs a
   witness whose 2x1/1x2 unit is NOT against a frame edge — a wider frame, or
   `--sb-size=128` with a 64x64 mu chunk boundary, where `of_tu`'s `wide`
   branch (`col_off % 64`) becomes reachable. Not encoded here.
2. **`decode_rect_split`'s 2x2 / `NxN` cell is unreachable**, so
   `RECT_TILED_CHROMA_NXN_HITS` is 0 on every admitted stream. It is kept so
   the `match` over grid shapes is exhaustive rather than silently
   default-shaped; it is not a coverage claim.
3. **The 4:2:2 `(4,8)` unwindowed site (decode.rs:12525) is argued unreachable,
   not proven so** — the argument is the sequence header's named refusal, and I
   did not run a 4:2:2 stream through the refusal to watch it fire on this
   branch.
4. **No 4:2:2 stream was encoded**, so nothing here re-verifies the
   `EC_AV1_ALLOW_422_PROBE` bypass or any 4:2:2 arm.
5. **The 4:4:4-LOSSY 2x2 cell of site 1 and site 2** (i.e. a skipped 64x64
   intra-BC block with a residual rather than pure prediction) is not
   distinguished from the skip cell by any pin here; `r512.obu`'s two
   multi-unit blocks are both `skip=1`.
6. **No VPS run.** Everything above is local: one gate at a time plus two
   ~250 s wide-family runs. Per the project's standing rule the full suite
   belongs on the fleet; I did not request or consume a fleet slot.
