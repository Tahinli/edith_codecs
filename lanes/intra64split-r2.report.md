# lane-intra64split r2 — RED (root cause found, and it is NOT this lane's shape)

## Verdict
RED. The gate still cannot fire the 64-level intra HORZ/VERT strip, and the r2
recipe uncovered a *different*, real, reproducible decode defect on the inter
path. Both are measured below; nothing was weakened to make anything pass.

## What changed
* `crates/ec-av1/src/decode.rs:1975` — `RECT64_CORNER_TU_HITS` is now
  `Cell<[usize; 2]>` keyed on orientation (0 = 64x32, 1 = 32x64);
  `rect64_corner_tu_hits(orient)` at `:1981`; bump site at `:~5985` uses
  `tx_h > tx_w`. A single counter cannot tell an axis swap from a working arm
  (class `[[scan-weights-cross-axis]]`).
* `crates/ec-av1/src/decode.rs:~5978` — `EC_RECT64TU` trace on the corner-TU
  arm (same idiom as `EC_SBPART_DUMP64`): `mi`, `tx`, `mode`.
* `crates/ec-av1/src/stream.rs:78` — public accessor takes `orient`.
* `crates/ec-av1/src/stream.rs:4404..` — gate recipe: 14 attempts (was 40),
  6 frames (was 8), `cpu-used` 2..4 (was 0..4 — cpu-used 0/1 was the 65-min
  wall in r1), two frame sizes 192x128 / 256x192, 7 cq levels, and the gate now
  asserts **both** orientations on pixel-compared streams.
* Merge of `main` 48b35ab; `cdf.rs`, `cdf_state.rs`, `mvstack.rs` byte-compared
  identical to main after the resolution.

## Refusals
None lifted. `"a split intra strip whose transform unit is {tx_w}x{tx_h}"`
stays in `refusal_inventory.rs:35`: the shapes it can still reach are the 1:4
units (32x8/8x32, 16x4/4x16, 64x16/16x64 at depth), so it is NOT vacuous — only
the 64x32/32x64 cells were removed from it in r1, and those are still ungated.

## MEASURED 1 — the charter's flag pair is the opposite of what the gate needs
`--min-partition-size=32 --max-partition-size=64` prunes libaom's rect
partition search entirely. Same 6-frame source, same everything else, aomdec
`EC_TRACE=1` partition histogram:

| recipe | PARTITION_HORZ/VERT @ BLOCK_32X32 | total partition symbols |
|---|---|---|
| base (no size range) | 8 | 1012 |
| base + `--min-partition-size=32 --max-partition-size=64` | **0** | 180 |

So the r1/r2 recipe could never produce a rect partition at all. Those flags are
now absent from the gate, with the numbers in a comment. Class
`[[gate-recipe-confound]]`.

EVIDENCE: /tmp/.../i64r2/ur1.log,ur2.log | aomenc 4 recipes on one y4m, aomdec EC_TRACE=1 | `grep -c 'bsize=9 value=[12]'` 8 vs 0, `grep -c EC_PART_VAL` 1012 vs 180

## MEASURED 2 — aomenc never picks a 64-level HORZ/VERT here at all
Across every recipe tried (mandelbrot fast zoom, 32-px horizontal noise bands,
32-px vertical noise bands, testsrc2; cq 12/20/30/40/55; 192x128), the count of
`bsize=12 value=1|2` (PARTITION_HORZ/VERT at BLOCK_64X64) is **0**, in ~3300
partition symbols. 64x64 blocks are only chosen where the content is flat, and
there PARTITION_NONE always wins. The 64-level strip *is* real (both films stop
on it) — the gate needs a source that makes a 64x32 strip win RD, and none of
the four candidate sources does.

EVIDENCE: /tmp/.../i64r2/tr{a,b,c,d}.log, ur{1,3,4}.log | 7 encodes, aomdec EC_TRACE=1 | `grep -c 'bsize=12 value=[12]'` = 0 in all

## MEASURED 3 — a real INTER defect the recipe exposed (NOT this lane's shape)
Attempt seed 48 (192x128, cpu-used 2, cq 40) decodes and MISMATCHES.
Stream pinned: `/tmp/.../i64r2/s48.obu`, md5 `add55f625b6302c523455bb8662b19f5`.
* Pre-deblock (`EC_AV1_PREFILT_DUMP`, ours vs instrumented aomdec): frames 0,1
  exact; frame 2 luma differs in **exactly** the 64x64 superblock at px(128,64)
  — bbox x128..191 y64..127, 3720 samples — and **chroma is exact** (U 0, V 0).
* That block is aomdec `EC_PART mi_row=16 mi_col=32 bsize=12 value=0`
  (PARTITION_NONE 64x64) and `EC_MODE_VAL ... mode=16 ref0=4 ref1=-1
  mv0=(-340,-116)` — an **inter** NEWMV block off GOLDEN with half-pel mv on
  both axes. Not an intra strip.
* Our `EC_RECT64TU` fires exactly once in the whole stream, at mi(0,16), i.e.
  AFTER that divergence — a hit counted from an already-desynced decode
  (class `[[counter-from-refused-stream]]` / `[[refusal-from-own-desync]]`).
  The gate's "64x32=1" line for seed 48 is therefore spurious; the pixel assert
  correctly failed the attempt before the counter could green anything.

EVIDENCE: /tmp/.../i64r2/{our,ref}.f2 + aom_part.log + aom_mode.log | ffmpeg lavfi -> aomenc -> our dump_yuv + aomdec EC_AV1_PREFILT_DUMP/EC_TRACE/EC_TRACE_MODE | frame 2 luma diff 3720 confined to px(128,64)+64x64, chroma diff 0, block = 64x64 PARTITION_NONE inter NEWMV ref0=4

## Deviations from the charter (named)
* `lane-r14 b86eb38` NOT merged. Reason: the ledger records that merging it
  deletes the "split transform on a 16x16-level 1:4 inter strip" refusal and
  lets a TX_16X4 -> TX_8X4 split reach `rect_inter_luma_set`'s `unreachable!()`
  (panic risk) until `TxbSet::LumaRect8x4Inter` exists. It is orthogonal to this
  lane's shape; taking a known panic risk into a red lane buys nothing.
  deferred: rect var-tx leaves for the HG 300 wall — needs the r14 panic guard.
* `lane-sqdrift 7d498c0` NOT merged: **not needed**. The no-CFL fix it carries
  is already on this tree — `decode.rs:6299` computes
  `let cfl_allowed = bw.max(bh) <= 32;` and reads `uv_mode_no_cfl` for a 64-axis
  strip. Verified by reading `decode_intra_rect_in_inter`, not by memory.
* Charter's `--min-partition-size=32 --max-partition-size=64`: dropped, with the
  measurement above. Charter's `--kf-max-dist=9999` applied (both kf dists).
* `--enable-tx-size-search=0` kept (r1's finding: `=1` stops earlier at
  lane-intrasplit's split-strip refusal, before this shape).

## Test totals
Full suite armed as unit `intra64split-suite-r2-*`, log
`$HOME/.cache/intra64split-suite-r2.log`. The r1 suite unit
(`intra64split-suite-1788329481`) was stopped; it had run 1h20m and never
emitted a `test result` line (its own slow gate was inside it) — no totals from
r1 exist.

## Residue
* fix-now (next round): a gate source that makes a 64-level HORZ/VERT strip win
  RD. The 64x64 block must be *large and flat enough to be chosen* yet split
  horizontally by content — e.g. a frame-tall flat gradient with one horizontal
  discontinuity every 32 rows at low cq, or a real 4K crop from the film.
* deferred(other lane): the inter 64x64 luma defect of MEASURED 3 — luma-only,
  chroma exact, pre-deblock, on `s48.obu` frame 2 mi(16,32). It is an inter-path
  defect, outside this refusal.
* deferred(tool budget): the HG `-ss 300` probe + `EC_AV1_FINAL_DUMP` count was
  not re-run this round (the suite holds the target dir); r1's result stands —
  the film now stops at "a split transform on a 1:4 inter strip with a 64-px
  axis".
