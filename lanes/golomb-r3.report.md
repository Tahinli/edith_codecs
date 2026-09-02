# lane-golomb r3 — the suite red was my own gate recipe; the edge strip was out-of-frame transform units

## TASK 1 — bisect table (the r2 suite failure)

`stream::tests::a_real_aomenc_stream_with_a_coded_rect_strip_below_16x16_decodes_pixel_exact`,
run in a detached scratch worktree (`scratchpad/verify-golomb`, `CARGO_TARGET_DIR=~/.cache/cargo-target-verify`):

| commit | what it is | result |
|---|---|---|
| a762fff | mcomp64 base | ok, 1 passed |
| 909064f | + fi8 cherry-pick 2d5c425 | ok, 1 passed |
| eacd7fd | r1 edge-bit fix | ok, 1 passed |
| fd54bcf | r2 guards | **FAILED** — "the stream decoded but no coded (non-skip) rect leaf fired" |

EVIDENCE: bisect stdout above | `git worktree add --detach` at each sha, `cargo test -p ec-av1 --lib -- a_real_aomenc_stream_with_a_coded_rect_strip_below_16x16` | 3 ok / 1 FAILED, first red = fd54bcf

Root cause: **not** fi8, not the r1 edge read. r2's per-arm recipe override
(`--min-partition-size=32 --tune-content=film`) was pasted into the WRONG gate — hunk
`stream.rs@8435`, inside `a_real_aomenc_stream_with_a_coded_rect_strip_below_16x16`, whose whole
point is sub-16x16 rect leaves. At `--min-partition-size=32` that 64x64 mandelbrot frame cannot
contain a 16x8 leaf, so `rect_leaf_coeff_hits` went to 0 and the gate's own
"the gate proves nothing" assert fired — the assert did its job. Fixed in ebec75d by restoring
`--min-partition-size=8`; the edge32 gate has always carried its own copy of the override.
No warning is owed to the fi8 merge agent: fi8 is already on main (b1d8457) and is exonerated by
row 2 of the table.

EVIDENCE: gate stdout | `cargo test -p ec-av1 --lib -- --nocapture a_real_aomenc_stream_with_a_coded_rect_strip_below_16x16` after ebec75d | `pixel-exact, rect_leaf_coeff_hits=8`, 1 passed

## TASK 2 — the frame-edge strip's wrong pixels: out-of-frame transform units

Class: `parsed-then-discarded`'s mirror — *read* something the reference never wrote
(`equal-range-means-unread`).

Reproduced standalone (192x80 cq40 8-bit flat band, md5 `4bd18b25…`, scratchpad `e32.obu`):
1081 Y pixels differ, rows 62..79, **cols 124..191** — the LAST superblock only.

The oracle's own view of that superblock row (instrumented aomdec):
`EC_PRED mi_row=16 mi_col=16 … bsize=11 part=1 txw=16 txh=16` with **row_off=0 only** (four TUs),
and `mi_row=16 mi_col=32 … bsize=11 part=1 txw=32 txh=32` (two TUs). `bsize=11` is
`BLOCK_64X32`: aomenc answers the bottom-edge superblock with the **64-level** `PARTITION_HORZ`,
never the 32-level one, and codes only the transform-unit ROW that is inside the frame.

Ours decoded `mi_col=16` identically (`TRACE_RECT_SPLIT bw=64 bh=32 tx=16`) but then walked
`bh/tx = 2` TU rows, reading coefficients for four phantom 16x16 units at y=80..95 — outside a
frame whose `mi_rows*4 == 80`. That desynced the stream at exactly the next symbol: the SB2
partition bit came out SPLIT (two 32x16 strips, `edge32=[2,0,2,0]`) where the oracle read HORZ.
The r2 report's reading ("prediction/availability inside the strip") was wrong; the strips
were the *symptom* of a desync, and the "32-level edge HORZ fired" counter was a
`counter-from-refused-stream`-shaped artefact of our own desync.

Fix — `crates/ec-av1/src/decode.rs:4818` (`decode_rect_split`, the only place a block hangs off
the frame): skip a transform unit whose top-left sample is outside `true_width`/`true_height`
(`mi_cols*4`/`mi_rows*4`), mirroring libaom `av1_foreach_transformed_block_in_plane`'s
`max_blocks_wide/high` clip (`mb_to_right_edge`/`mb_to_bottom_edge`). The chroma unit of a strip
always has its top-left inside, so it is unaffected.

EVIDENCE: scratchpad/{e32.obu, ours.yuv, ao.yuv, ao-pred.txt, ours-step.txt} | instrumented
aomdec `EC_PRED`/`EC_TRACE_MODE_STEP` vs `decode_probe`, byte diff of the two raw yuv420p dumps |
before: 1081 Y pixels differ (first diff at our `EC_ISTEP mi_row=16 mi_col=32 name=skip rng=51413`
vs oracle `rng=32901`); after: **0 differing bytes over all three planes**

## Counters + the pin, un-ignored

`EDGE32_HITS` widened 4 → 8 slots (`decode.rs:1602`): slots 4/5 = the **64**-level gathered edge
bit read as HORZ/VERT / SPLIT (both tile paths), 6 = a 64x32 bottom-edge strip decoded with
`has_rows == false`, 7 = a 32x64 right-edge strip. Necessary because the flat-band arm's r2
assert (`totals[2] > 0`, a 32-level bottom-edge HORZ) was only ever satisfied by our own desync;
on a correct decode of this content the 32-level slots are 0 by construction and the 64-level
slots carry the proof. `#[ignore]` removed from
`a_32x32_frame_edge_rect_partition_with_a_flat_band_decodes_pixel_exact`.

EVIDENCE: gate stdout | `cargo test -p ec-av1 --lib -- --nocapture a_32x32_frame_edge` |
2 passed; flat arm: 32 pixel-exact attempts, 64-level edge bits [horz_or_vert=96 split=0]
bottom-HORZ=48 right-VERT=48, 32-level slots all 0; detail arm: 32 pixel-exact attempts,
64-level [horz_or_vert=7 split=89] right-VERT=7, 32-level split=178

## Suite
`$HOME/.cache/golomb-suite-r3.log` — **339 passed / 0 failed / 31 ignored** (277 s), run as a
user systemd unit on the final tree. r2 was 337/1 FAILED/32 ignored: +1 fixed (task 1), +1
un-ignored (task 2).

Siblings re-run inside that suite, all green: `a_real_aomenc_stream_with_a_coded_rect_strip_below_16x16`,
`the_hunger_games_head_key_frame`, both `a_*_32x32_frame_edge_*`, `tiny_frame_size_sweep`,
`superblock_level_*`, `filter_intra_*`, `refusal_inventory`, `gate_coverage`.

## Same-shape sweep
`decode.rs:7458` (`decode_block`'s square multi-TU loop) has the identical unclipped
`0..n_axis` shape and now carries the identical guard. A square block only hangs off the frame
when the frame's mi dimensions are odd at that block's level (lane-oddh's territory), so no gate
in this suite reaches it: the guard is libaom's rule applied by inspection, proven only inert
here (suite unchanged). `decode_block_rect64` delegates its split case to `decode_rect_split`,
so it is covered by the measured fix. Flagged, not claimed.

## Film
`hg-head.obu` (18 frames) still stops at
"an inter SB-level partition type other than NONE or SPLIT (this decoder's inter tile path
recurses a superblock only as SPLIT)" — unchanged by this round (same string as r1's residue;
the inter tile path never reaches a 64-level HORZ).
EVIDENCE: probe stdout | `cargo run -p ec-av1 --example decode_probe -- hg-head.obu` | REFUSED at
the inter SB-level partition, 18 frame headers parsed, tiling cols=1 rows=1

## Residue
- deferred: the inter tile path's 64-level `PARTITION_HORZ`/`VERT` (the film's next blocker) —
  unblocked by rect inter residual coding, which is lane-inter*/interbis territory, not this one.
- accepted: the square-block guard above is unexercised by any gate on this lane (odd-mi
  geometry is lane-oddh's gate).
