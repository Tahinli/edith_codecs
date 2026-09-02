# lane-obmcrec r1 — OBMC neighbour with no recorded interp filter

Branch `lane-obmcrec` (base main `beecb64`), commits `15df221`, `32d9965`.

## Measurement (step 1)
New env rung `EC_OBMCREC=1` (decode.rs `obmcrec_probe`, off by default) prints every OBMC
neighbour whose recorded filter symbols are the `[3, 3]` sentinel, plus the grid cell the
filter was read from. On the three 2 s cuts of the 10-bit 1920x792 128-SB stream:

| cut | current block | neighbour taken | filter cell read | cell state |
|---|---|---|---|---|
| t900 | left, mi(24,348) 16x16 | 16x4, ref 1, mv(-84,18) | mi row 24 | intra, ref -1 |
| t5400 | above, mi(150,340) 8x8 | 4x8, ref 7 | mi col 340 | intra, ref -1 |
| t6300 | above, mi(56,32) 16x32 | 4x16, ref 1 | mi col 32 | intra, ref -1 |

`fixed=false` (switchable frame) in all three; each is the FIRST such neighbour of the cut.

EVIDENCE: ~/.cache/rectres-tmp/{t900,t5400,t6300}.obu | EC_OBMCREC=1 decode_probe per cut |
3 sentinel hits, all with an INTRA cell under an INTER MiInfo

## Root cause
libaom `foreach_overlappable_nb_above`/`_left` (reconinter.c 780/840) snap a 4-wide/4-tall
neighbour back to its chroma pair's even mi and then take the neighbour's whole `mbmi` --
mv, reference AND `interp_filters` -- from the pair's **second** mi (`above_mi = prev_row_mi
+ above_mi_col + 1`). Ours snapped the `MiInfo` the same way but read the switchable filter
symbols from the band cell of the **first** mi, a different block; whenever that half is
intra its filter record is the `[3, 3]` "no filter" sentinel and `neighbour_filter` refused.
Fix (decode.rs): `overlappable_above`/`overlappable_left` return the mi the `MiInfo` came
from (`src4`), and `obmc_blend` reads `above_filter[mi_col + src4]` / `left_filter[mi_row +
src4]`. No other record path was missing a filter write (every `record_inter_rect_mi` call
site passes a resolved value; `interp_fixed` frames are handled ahead of the sentinel).

## Gate
`a_real_aomenc_10bit_obmc_over_1to4_strip_neighbours_decodes_pixel_exact` (stream.rs) --
real aomenc, 10-bit 128x128, 24 frames, `--enable-obmc=1 --enable-1to4-partitions=1
--min-partition-size=4 --enable-rect-partitions=1` (+ noise fixture), seeds 43/46/47/65/66,
every decode-order frame all three planes exact vs ffmpeg, hard assert on the new
`obmc_pair_filter_hits` counter (neighbours sourced from a pair's second mi).

`EC_NOMEMGUARD=1 EC_AV1_REQUIRE_AOMENC=1 CARGO_TARGET_DIR=$HOME/.cache/cargo-target-obmcrec cargo test -p ec-av1 --lib -j3 -- obmc sub8 refusal_inventory gate_coverage`
-> `25 passed; 0 failed` (incl. the new gate and every sibling OBMC/sub8 gate).

EVIDENCE: cargo test -p ec-av1 --lib -- obmc sub8 refusal_inventory gate_coverage |
new gate + 24 siblings | 25 passed 0 failed, obmc_pair_filter_hits fired on 5 seeds

Seed sweep 42..=73 (32 real encodes) picked the seeds: 43/46/47/65/66 fire and are exact;
70 and 73 fire and mismatch, but mismatch IDENTICALLY with `--enable-obmc=0` (seed 73 frame
1 luma, seed 70 frame 16) -> a `--min-partition-size=4` recipe defect, NOT OBMC.

## Refusals
- "an OBMC neighbour whose switchable interp filter was never recorded" — RETAINED (not
  lifted): libaom has no such state, so the string is now only an upstream-desync guard.
  Unreachable on all four film cuts and on the gate's 32 sweep encodes.
- "an intra-coded 16x4/4x16 strip on the inter block path (...)" — RENAMED to
  "an intra-coded {bw}x{bh} block on the inter block path (no size-group/tx-category row for
  that shape here)". Measured: every arrival is a **128x64** intra block inside an inter
  frame (mi (128,416)/(192,0)/(192,0)) — the old text named a correlate. refusal_inventory
  updated in the same commit.

## Film cuts (10-bit 1920x792 128-SB, 2 s each), after
| cut | before | after |
|---|---|---|
| t900 | OBMC neighbour filter never recorded | intra-coded 128x64 block on the inter block path |
| t5400 | OBMC neighbour filter never recorded | intra-coded 128x64 block on the inter block path |
| t6300 | OBMC neighbour filter never recorded | a Golomb tail longer than this decoder reads |
| t8100 | (already) intra 16x4/4x16 on inter path | intra-coded 128x64 block on the inter block path |

## Suite
`systemd-run --user --unit=obmcrec-suite-r1-1788365976 ... cargo test -p ec-av1 --lib -j3`
-> `424 passed; 1 failed; 37 ignored` (log `$HOME/.cache/obmcrec-suite.log`, 667 s).
The single failure `real_aomenc_1to4_streams_decode_pixel_exact_and_rect_vartx_leaves_fire_before_a_named_refusal`
is PRE-EXISTING: the same test fails with the identical message ("the rectangular var-tx
leaf arm never fired (32x16=0, 16x32=0, 0 refusals, 0 compared)") on the base commit
`beecb64` in a detached verify worktree. Not caused by this lane (this diff changes only
prediction pixels, never entropy decode, so it cannot move a leaf-shape counter).

## Residue
- deferred: intra 128x64 (128-SB HORZ half coded intra in an inter frame) — needs a
  size-group/tx-category row + 64x64-TU reconstruction + its own gate — unblocks the three
  1080p cuts' next wall. Own lane.
- deferred: t6300's Golomb-tail refusal — separate site, untouched here.
- deferred: `--min-partition-size=4` 10-bit chroma/luma mismatch at seeds 56/70/73
  (reproduces with `--enable-obmc=0`) — a sub-8x8 recipe defect for another lane.
- accepted: the OBMC sentinel refusal string stays as a desync guard.
