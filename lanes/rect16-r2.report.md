# lane-rect16 r2 report

## What 34827e1 (builder's in-flight commit) actually contained
- Discovered mandelbrot's real first refusal was `PARTITION_VERT_B` at mi=(3,2), not plain HORZ/VERT
  (the lane's original charter premise). Wired VERT_B: left 8x16 strip via existing `decode_block_rect`
  (skip-only, at its natural SUB-grid position) + two 8x8 leaves via existing `decode_leaf8`.
- Added a gate test `a_real_aomenc_stream_with_mandelbrot_fires_the_vert_b_partition_arm` asserting
  `vert_b_intra_hits() > 0`.
- BUG: that gate's aomenc invocation was missing `--passes=1` (every one of the other 42 aomenc
  recipes in stream.rs has it) -- aomenc silently ran 2-pass and pass 1 wrote an EMPTY stream to
  stdout, still exit 0. So the gate always decoded zero bytes and never actually fired anything;
  it was vacuous, and it is the 1 failing test in the pre-merge suite (266 passed/1 failed).

## Premise re-check
Confirmed still-live with `decode_probe` against a real aomenc stream (`--cpu-used=4 --end-usage=q
--cq-level=32 --passes=1`, sb-size=64, threads=1): before my fix, mandelbrot still stopped at
`REFUSED: ... (a partition below 16x16 other than a clean split or a VERT_B ...)`. Added a
`EC_RECT16_DEBUG` temporary eprintln (removed) to find WHICH part16 value: **`part16=1`
(`PARTITION_HORZ`) at mi=(0,7)`** -- the mandelbrot blocker is a plain HORZ, decode-order-earlier
than VERT_B's mi=(3,2), even though VERT_B's own doc claimed "frame 0's very first non-NONE/SPLIT
symbol" (that claim was about z-order recursion, and was simply wrong once traced against real
decode order). So the ORIGINAL lane charter target (plain HORZ/VERT below 16x16) was correct all
along; the r1 VERT_B work was real and worth keeping, just not the frontmost blocker.

## What I built
New `decode_leaf_rect` (crates/ec-av1/src/decode.rs), sibling to `decode_leaf8` the way
`decode_block_rect` is sibling to `decode_block`: takes REAL mi coordinates (not derived from an
SUB-grid `outer_at`), so it can address a HORZ/VERT split's offset second strip, which
`decode_block_rect`'s `px = c * SUB` formula cannot name (that was VERT_B's r1 doc-noted gap).
Uses `read_intra_mode_rect` for mode/skip/tx-depth, `record_mi_rect`/`fill_skip_grid_rect`/
`fill_lf_grid_rect` for fine-grained neighbour bookkeeping (never the coarse `record_rect`), and
`prev_leaf` chaining exactly like `decode_leaf8` for the second strip's above/left context.
Wired into `decode_key_frame_tile_with_cdfs` next to the VERT_B arm: `PARTITION_HORZ` -> two 16x8
strips (top/bottom, mi row step 2); `PARTITION_VERT` -> two 8x16 strips (left/right, mi col step 2).
New hit counter `horz_vert_intra_hits()` (thread-local `Cell<usize>`, same pattern as
`vert_b_intra_hits`).

## Ceiling hit
Only the SKIP case is ported (matches `decode_block_rect`'s own precedent at this size range and
VERT_B's own left-strip precedent): a coded (non-skip) HORZ/VERT strip below 16x16 has no
rectangular-transform coefficient tables here yet (same square-only-transform ceiling as the
sibling `lane-sub8`/`decode_block_rect` non-skip refusal at (16,8)/(8,16)), refused by name:
`"a coded (non-skip) HORZ/VERT rect strip below 16x16 (this decoder ports only the skip case at
this size)"`. Filter-intra on this strip is also refused by name (mirrors the existing
`decode_block_rect` refusal). Both added to `refusal_inventory.rs`'s pinned `REFUSALS` list.
Generic multi-way refusal string updated to `"a HORZ_A/HORZ_B/VERT_A partition below 16x16 ..."`
(HORZ_A/HORZ_B/VERT_A remain out of scope this round) and repinned.

## Live mandelbrot re-probe after the fix
Decode now gets PAST mi=(0,7)'s HORZ arm entirely (structurally proven -- reads real symbols,
reconstructs) and stops at the coded-strip ceiling: `REFUSED: ... (a coded (non-skip) HORZ/VERT
rect strip below 16x16 ...)`. Tried cq=20/40/50/63 too: 20 hits the same coded-strip ceiling,
40 hits an unrelated pre-existing "partition below 8x8" refusal, 50/63 hit the filter-intra-on-strip
refusal (also newly wired, proving it reads real symbols too). No recipe reached a pixel-exact full
decode of this stream -- consistent with VERT_B's own r1 finding, not a regression I introduced.

## Gate
`stream::tests::a_real_aomenc_stream_with_mandelbrot_fires_the_vert_b_partition_arm` (kept its old
name to avoid extra churn; doc comment rewritten to explain r1 vs r2 findings): fixed the missing
`--passes=1` (now asserts `!stream.is_empty()` as a guard against the same vacuous-gate class
recurring), swapped its hard assertion from `vert_b_intra_hits() > 0` (never true on this stream
post-fix, since HORZ blocks decode first) to `horz_vert_intra_hits() > 0`.
`EVIDENCE: cargo test -p ec-av1 --lib -- a_real_aomenc_stream_with_mandelbrot_fires_the_vert_b_partition_arm --nocapture | horz_vert_intra_hits=1, vert_b_intra_hits=0, decode result: Err("... coded (non-skip) HORZ/VERT ...")`

## Suite / check
`cargo test -p ec-av1 --lib -j4 -- --test-threads=4`: 267 passed, 0 failed, 20 ignored (was 266/1
pre-fix, the 1 failure being the vacuous gate above).
`cargo check --workspace --all-targets -j4`: clean, no errors, only pre-existing warnings.
HEAD: 7a49307 (branch lane-rect16).
