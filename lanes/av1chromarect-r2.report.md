# lane-av1chromarect-r2 — localising the 79969-sample residual

Worktree `~/.cache/wt/av1chromarect2`, branch `lane-av1chromarect-r2`, base
`5324a685` ("Merge lane-av1chromarect"). No decode-path change is committed: the
residual is a **different defect class in a function this lane does not own**, and
the yield is the narrowed statement below, with every number measured here.

All probes were temporary and are reverted; `git status` is clean and the main
checkout is untouched.

## 1. What the residual IS

Same oracle as r1 (aomdec `EC_AV1_FINAL_DUMP` vs `set_final_dump_prefix`,
512x512 8-bit 4:4:4, decode order), reproduced on this base:

| frame | Y | U | V |
| --- | --- | --- | --- |
| 0 | 0 wrong (byte-exact) | **7488 wrong from 229728** (x=352, y=448) | **8128 wrong from 229728** |
| 1 | 0 wrong (byte-exact) | **22571 wrong from 65704** (x=168, y=128) | **41782 wrong from 65704** |
| 2 | 0 wrong (byte-exact) | 0 | 0 |

Total 79969 of 2359296 samples (3.4 %), all chroma, luma exact everywhere.

Per-frame, the strip footprints (`EC_HALVSWEEP=1`, `mi` in 4-px units,
`px = mi * 4`) and the dirty-cell map (`{x}/{wrong samples in that 8x8 cell}`):

    f0  strip mi=(32,40)  px=(160,128) 32x64 skip=false   <-- the only CODED strip
    f1  strip mi=(48,64)  px=(256,192) 32x64 skip=true
    f1  strip mi=(64,108) px=(432,256) 16x64 skip=true
    f2  strip mi=(112,96) px=(384,448) 32x64 skip=true
    f2  strip mi=(112,104) px=(416,448) 32x64 skip=true

* f0 dirty cells: x = 352,360,368,376(24),416(48),424(32),432..504, y = 448..504.
  The coded strip's footprint is x=160..192, y=128..192 — **clean**.
* f1 dirty cells: x = 128..192 for y=192..504, plus x=192..280 for y=128..168,
  plus x=352..368 for y=448..504. Strip 1's footprint is x=256..288, y=192..256
  — **clean**; strip 2's is x=432..448, y=256..320 — **clean** (x=432 is inside
  f1's V bbox but its U cells are clean).
* f2 is byte-exact on all three planes and carries two strips.

**All five strips are pixel-clean on their own frames.** The residual is not the
new walk, and it is not "a second strip on the same plane block" that the walk
missed: on this witness the multi-unit walk (`read_intrabc_rect_chroma_split`)
runs exactly once, for f0's single coded strip, and it reads four units
(`EC_IBCCU`: plane 1 at (160,128) and (160,160), plane 2 at the same two
positions) whose footprint is exact.

## 2. Attribution — the seed unit

`EC_IBBLK` (every `decode_block` / `decode_block_rect` entry, frame-tagged) puts
every dirty cell inside a **square** block decoded by `decode_block`, at 4:4:4,
`side=64`, `cset=Chroma32 ctx=32 ctxh=32 cside=64 cheight=64` — i.e. a 64x64
luma block whose 64x64 chroma plane block is walked as a 2x2 grid of
`Chroma32` units, in the SQUARE path, which this lane never touched.

The f0 seed, with the oracle's own mode rung as the control
(`EC_TRACE_MODE=1` on `~/.cache/aom-oracle/build/aomdec`):

    ours  IBM f=0 mi=(112,80) px=(320,448) side=64 skip=true mode=0 uv=0
    ours  IBM f=0 mi=(112,96) px=(384,448) side=64 skip=true mode=0 uv=0
    ours  IBM f=0 mi=(112,112) px=(448,448) side=64 skip=false mode=0 uv=0
    ours  IBM f=0 mi=(112,64) px=(256,448) side=64 skip=false mode=12 uv=12

    oracle  EC_IMODE_VAL mi_row=112 mi_col=80  mode=0 uv_mode=0 skip=1 tx=0
    oracle  EC_IMODE_VAL mi_row=112 mi_col=96  mode=0 uv_mode=0 skip=1 tx=0
    oracle  EC_IMODE_VAL mi_row=112 mi_col=112 mode=0 uv_mode=0 skip=0 tx=0
    oracle  EC_IMODE_VAL mi_row=112 mi_col=64  mode=12 uv_mode=12 skip=0 tx=0

**Geometry, mode, uv_mode, skip and tx all agree with the oracle.** This is not
a bitstream or partition difference; it is a reconstruction difference on a
block both decoders agree exists with the same flags.

f0 U, y=448, x = 256 272 288 304 320 336 | 352 368 | 384 400 416 | 432 448 464 480 496:

    oracle: 192 241 241 241 241 239 | 202 202 | 202 202 203 | 165 166 166 166 166
    ours  : 192 241 241 241 241 239 | 241 239 | 202 202 202 | 202 175 175 171 171

The agreement region ends exactly at x=352 = 320 + 32, the 4:4:4 chroma plane
block's 2x2 grid boundary. The oracle's block [320,384) is 241 on its left unit
and 202 on its right unit; ours is ~241 on both. The neighbouring
`mi=(112,96)` block [384,448) agrees. The coded `mi=(112,112)` block [448,512)
is wrong throughout (202/175 against 165/166) — the victim of whatever the seed
leaves behind.

f0 V, same row, is the mirror image (ours 110/110 against 222/222 on the seed's
right unit), so both chroma planes are wrong in the same unit.

## 3. The mechanism, as far as it is pinned

The seed is `skip = true`, `mode = uv_mode = 0` (DC_PRED), so its chroma is pure
PREDICTION with a zero residual — the error is in the prediction, not in a
coefficient read. Its 2x2 chroma walk's edge samples, read from the plane just
before each unit is pushed (`EC_IBSKIP`, plane U):

    mi=(112,80) px=(320,448)  [the dirty block]
      cu=(0,0) xy=(320,448) left=241 above=241 corner=241
      cu=(1,0) xy=(352,448) left=202 above=194 corner=118
      cu=(0,1) xy=(320,480) left=241 above=202 corner=241
      cu=(1,1) xy=(352,480) left=202 above=202 corner=202
    mi=(112,96) px=(384,448)  [the clean block, control]
      cu=(0,0) xy=(384,448) left=202 above=202 corner=152
      cu=(1,0) xy=(416,448) left=202 above=203 corner=163
      cu=(0,1) xy=(384,480) left=202 above=203 corner=202
      cu=(1,1) xy=(416,480) left=202 above=203 corner=165

The clean block's edges are all ~202, so any DC rule reproduces the oracle. The
dirty block's are NOT: its right unit has an above row at 194 and a left column
whose first sample is 202, yet it reconstructs flat 241 — the sibling unit's
value. **So the second chroma unit of a 4:4:4 64x64 square block's 2x2 chroma
walk does not get its own DC prediction; it gets its sibling's.**

That is the narrowed mechanism. The site is `decode_block`'s skip/palette chroma
loop (`crates/ec-av1/src/decode.rs`, the `for cu_row { for cu_col { push_intra_unit
(1, cu_x, cu_y, chroma_tx, chroma_tx_h, uv_predict_mode, …) } }` walk and its
plane-2 twin, followed by the whole-block `neighbours.record`) — a prediction
that is resolved per BLOCK where libaom resolves it per UNIT
(`av1_predict_intra_block` is called per (plane, transform unit) with the unit's
own above row and left column; `av1_build_dc_predictor_sb`). I did not pin it to
a line inside the predictor: `push_intra_unit` -> `push_intra` -> `inline_intra`
-> `exec_intra` takes the unit's own `x, y, bw, bh`, so the block-level reuse is
either upstream of that call or inside `exec_intra`'s DC arm, and separating
those needs a per-unit DC trace on both sides that I did not build.

## 4. Ruled out, by measurement

* **The new walk.** All five strips pixel-clean on their own frames (§1); the
  four `EC_IBCCU` units are exact.
* **The extent / coefficient set.** `cset=Chroma32 ctx=32 ctxh=32` is
  `get_vartx_max_txsize`'s `av1_get_adjusted_tx_size(TX_64X64) = TX_32X32`, the
  same conclusion r1 reached for the strip arm, and it is the set already in use.
* **CfL.** `cfl=false` on all four bottom-row 64x64 blocks, and the r1 probe
  found zero `cfl_ac_q3_at` calls on the whole witness.
* **The skip arm's coefficient-context record.** libaom's
  `av1_reset_entropy_context` zeroes a skipped block's footprint on every plane;
  this crate's `record_mi_rect` writes all three planes over the luma cells
  (`fill_span(&mut self.left, mi_r, luma_h, states)` with `states: [Neighbour; 3]`)
  and the chroma-only tail is empty at 4:4:4, so the zero stamp DOES land.
  Forcing an extra per-unit zero `record_mi_chroma` into the skip arm changed
  **no** number in any frame or plane.
* **A stale/partition/bitstream difference.** The oracle's own `EC_TRACE_MODE`
  rung agrees with our decode on the seed's bsize, mode, uv_mode, skip and tx.

## 5. What is left, and where the next lane starts

`decode_block`'s square-path DC_PRED chroma, second unit, on a 4:4:4 64x64 block
(`cn_cols == cn_rows == 2`, `chroma_tx == chroma_tx_h == 32`, `uv_predict_mode ==
DC_PRED`, skipped or palette). f1's larger region is very likely the same defect
seen with more instances — f1 has a skipped 64x64 at `mi=(32,32)` px=(128,128),
the only 64x64 block in the dirty x=128..192 column, and its first dirty cell is
(168,128), 40 px into that block.

The reproduction is cheap and needs no new encode: `fixtures/r512.obu`, decode,
compare per plane against aomdec, look at frame 0 U from sample 229728. A gate for
it would want a 4:4:4 stream with a skipped 64x64 block and 4:4:4
`--min-partition-size=64`, which this witness happens to contain.

NOT DONE: the line inside the DC predictor. I did not build the paired
per-unit-DC trace (ours + oracle) that would separate "the block-level DC is
computed once and reused" from "the unit's edges are gathered over the wrong
extent" — those two need different fixes and I could not tell them apart from
the edge samples alone.

Also untouched here, as instructed: the 4:2:0 and 4:4:4-lossless arms. No gate
family was re-run on this branch because no code changed on it.
