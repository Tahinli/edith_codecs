# lane-troykf r2 report

Base `lane-troykf` bb6ebe5 (r1). Merged main 85887c7 (lane-mvtwin) at 4667015.

## What changed
* `crates/ec-av1/fixtures/troy_kf2700.obu` (NEW, tracked, 36 kB) — a 1920x792
  10-bit AV1 key frame from one of the user's own films, truncated to its first
  temporal unit. The only stream in this repo containing a **skipped
  `UV_CFL_PRED` block**; the instrumented oracle counts 5 of them.
* `crates/ec-av1/src/stream.rs:2975` —
  `a_real_film_key_frame_with_a_skipped_cfl_block_decodes_pixel_exact`: decodes
  the fixture, HARD-asserts `troy_chroma_counters().0 > 0` (skipped CfL) BEFORE
  any ffmpeg dependency, then compares all three planes against
  `ffmpeg -f obu -i - -f rawvideo -pix_fmt yuv420p10le -`.

This closes r1's open item: defect 1 (a skipped intra block must still predict
with CfL) now has a gate that fails if the fix is reverted.

## Gate hunt for a synthetic recipe: 18 more recipes, still zero
r1 swept 20 aomenc recipes without producing a skipped CfL block. r2 swept 18
more, detecting the block **oracle-side** (`EC_TRACE_MODE=1 aomdec` →
`EC_IMODE_VAL ... uv_mode=13 skip=1`), which needs no decode of ours:

| family | recipes | CfL blocks | skipped CfL |
|--------|---------|-----------|-------------|
| `geq` sin/X chroma-correlated stripes, cq 40..63, 8+10 bit | 6 | 0..3 | 0 |
| mandelbrot/testsrc2 with `cb=128+0.35*(lum(2X,2Y)-128)`, cq 40..63 | 6 | 1..117 | 0 |
| smooth ramp / low-frequency `sin(X/60)cos(Y/70)`, cq 45..63 | 6 | 0 | 0 |

Result (measured, not inferred): aomenc DOES pick CfL freely on
chroma-correlated content (117 blocks at cq 40), but never with `skip=1` — the
two conditions pull against each other (skip needs a zero LUMA residual, CfL
needs luma AC worth scaling). His film reaches it because directional intra on
real texture predicts exactly at a 1:4 leaf. Script:
`$HOME/.cache/troykf-work/cflsweep.sh <tag> <lavfi> <cq> <depth> [cpu] [maxpart] [minpart]`.

## Deviation from the charter
The charter asked for stored per-plane **sha256 constants**. The crate has no
sha256 (and adding a hash dependency for one fixture is not warranted), and the
sibling film fixture test `the_hunger_games_head_key_frame_decodes_pixel_exact`
already pins against a live `ffmpeg` decode. I used that same helper. Vacuity is
covered the other way round: the decode and the `skip_cfl > 0` assert run
unconditionally, only the pixel compare is behind `have_ffmpeg()`.

