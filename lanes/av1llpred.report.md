# lane-av1-llpred: the min-partition-8 lossless 4:4:4 chroma OBMC blend is ss-blind

## Ticket

Remeasure the 2772 chroma pixel diffs the llintra8 lane named (entropy-exact,
luma-exact), name the FIRST differing sample, and fix it only if one index
explains it. `decode_leaf8`'s palette set untouched (another branch), the
reserved 4:2:0 group-tail chroma arm untouched.

## Remeasure (this tree @ b3f815c8, BEFORE the change)

Same pinned fixture `/tmp/llintra8/testsrc2_min8.obu` (testsrc2 128x96
yuv444p, aomenc `--profile=1 --lossless=1 --enable-palette=0
--enable-intrabc=0 --min-partition-size=8 --max-partition-size=64`, 6
frames), fresh `aomdec --rawvideo` of the same bytes: **2772 chroma diffs
reproduced exactly** (frame 0 and all luma planes clean). But the parent's
three named sites did NOT hold: the first differing sample is

> frame 1, plane 1 (U), x=88 y=4 (mi row 1, col 22), ours=149 ref=150

— an inter 8x8 leaf at mi(0,22) with mv=(0,11), not any of mi(0,16),
mi(2,24) or mi(8,28) as named. Error maps (residuals are equal by
entropy-exactness, so final diff = prediction diff) showed narrow
column/row fragments inside blend regions — OBMC shape, not whole-block MC
or intra-edge shape.

## Root cause: one class, two sites, both "the 4:2:0 half hardcoded"

1. `obmc_run` (decode.rs ~30085/30101/30113) sized the CHROMA neighbour
   prediction and blend windows with literal `/ 2` (cbw/cbh/cox/cw and
   cbh/coy/ch). libaom sizes them per plane with the PLANE's own
   subsampling shift (`dec_build_prediction_by_above_pred`:
   `(op_mi_size * MI_SIZE) >> pd->subsampling_x`, blend bounds likewise),
   which is `>> 0` at 4:4:4 — the chroma blend covered half the columns at
   half the offsets. Luma was never affected (luma exact everywhere).
2. `obmc_plan`'s `skip_chroma_above` hardcoded the luma shape list
   `(8,8)|(16,8)|(8,16)`. libaom's `av1_skip_u4x4_pred_in_obmc`
   (reconinter.c:829) switches on the PLANE block
   (`get_plane_block_size`): only 4x4/8x4/4x8 skip the ABOVE pass's chroma
   (`return dir == 0`). At 4:4:4 the plane block is the luma block itself,
   so every 8x8/16x8/8x16 OBMC block was silently missing its above-pass
   chroma blend — the (96..101, y8..10) family on leaf mi(2,24).

Both fixed to derive ss (from `chroma_side` in `obmc_plan`, `ss_x/ss_y(fctx)`
in `obmc_run`); value-identical at 4:2:0 (`/2 == >>ss`). `obmc_run` is shared
by the 16x16+ square path and the 8x8 leaf path, so one fix covers both.
The `obmc_mask` table was already complete (M1..M32).

## Verification (this tree @ HEAD + the change)

- Fail-before → fix-after measured in steps: 2772 chroma diffs → 1126
  (geometry fix alone) → **0** (skip fix): decode output is byte-exact
  against fresh `aomdec --rawvideo` for ALL 6 frames, all planes
  (`cmp`-equal 221184 bytes). The stream-exactness claim is now TOTAL for
  this fixture.
- 4:2:0 non-regression: `cargo test -p ec-av1 --lib a_lossless` → 6 passed,
  0 failed (both 4:2:0 lossless libaom gates, the 444 min-partition-64
  inter gate, the sb128 pair, the 16x4 pair).
- `cargo check -p ec-av1 --all-targets`: 0 warnings, 0 errors (target dir
  `$HOME/.cache/cargo-target-av1llpred`).

## Named, not chased (spoken)

1. `EC_MCB` rung (decode.rs obmc_run) still windows its own dump with
   `write_w / 2` — a lane-t900 4:2:0-era INSTRUMENTATION assumption only;
   output-inert, decode unaffected. fix-now(deferred to whoever next
   touches that rung).
2. The llintra8 report's three named sites (mi(0,16)/mi(2,24)/mi(8,28))
   did not match a fresh run of the same bytes; mi(2,24) and mi(8,28) do
   appear in the diff set but the FIRST miss was mi(0,22). Lesson recorded:
   reremeasure before owning.

## Note on the shared oracle

All comparisons against fresh `$HOME/.cache/aom-oracle/build/aomdec
--rawvideo` output of THE SAME pinned bytes. The oracle tree has no
`EC_OBMC` rung (the decoder-side comment claiming a byte-matching oracle
format is aspirational); EC_PREDND covers intra only. Fixtures stay in
/tmp/llpred + /tmp/llintra8 (regenerable recipes above); nothing committed
from fixtures/.
