# lane-rectsplitx r3 report

Branch `lane-rectsplitx`, commit `58149c8` on top of r2 `eec6a40` (base main
`ce05d5f`; main has since moved to `11633d7` -- **not rebased this round**, see
"Open").

## Root cause (one line of code)

`crates/ec-av1/src/decode.rs:5665` -- `decode_leaf_rect`'s SPLIT arm built
`RectStripModes { .., smooth_neighbor_uv: false }`, hard-coded, while the
unsplit arm two dozen lines below passes the real
`neighbours.smooth_uv_neighbour(leaf_mi.0, leaf_mi.1, r, c)` computed at
`decode.rs:5637`. So every tx-split 16x8/8x16 leaf predicted its chroma with
the intra-edge FILTER TYPE off: luma bit-exact, chroma off by +-1..2 across
the block, with the error leaking into the blocks below/right.

Class: a split/unsplit pair where the split arm drops a field the unsplit arm
passes (sibling of `cdf-row-held-constant` -- a value pinned to a constant
where the reference varies it). Sweep: all four `RectStripModes` literals
(`decode.rs:5248`, `5656`, `5905`, `6250`) now pass the computed value; that
was the only constant one.

The charter's suspects (chroma TU geometry, CfL, TX_16X4 dequant/inverse) were
all wrong: the failing block is not a 32x8 1:4 strip. `EC_AV1_TRACE` puts a
`TRACE_RECT_SPLIT mi_row=4 mi_col=8 bw=16 bh=8 tx=8x8` (and its twin at
mi_row=6) exactly over the wrong region -- a 16x16 block split PARTITION_HORZ
into two 16x8 strips, each with `tx_depth=1`, chroma 8x4.

## Evidence

EVIDENCE: scratchpad/r3/s46.obu (sha256 31de47355a098b460975ca0156e0b10cebaacc3c1614d2a8791ece3440c8ba45, generated TWICE identical) | aomenc 192x128 8-bit cq12 seed46 `--enable-tx-size-search=1 --enable-1to4-partitions=1`, `EC_AV1_PREFILT_DUMP` ours vs instrumented aomdec | before: Y 0 diffs, U 32 diffs (all inside chroma cell x16..23 y8..15), V 173; after: `cmp o.f0 r.f0` BYTE-IDENTICAL (36864 bytes)
EVIDENCE: scratchpad/r3/trace.txt | `EC_AV1_TRACE=1 decode_probe s46.obu`, grep mi_row=4..7 mi_col=8..11 | `TRACE_RECT_SPLIT mi_row=4 mi_col=8 bw=16 bh=8 tx=8x8` == luma (32,16), the exact 8x8 chroma cell that was wrong

## Gates

`cargo test -p ec-av1 --lib -- <3 names>` (EC_AV1_REQUIRE_AOMENC=1):
`test result: ok. 3 passed; 0 failed` in 21.4s --

- `a_real_aomenc_stream_with_a_32x32_level_1to4_partition_decodes_pixel_exact`
  (RED as committed in r2, the round's target) -- GREEN, still hard-asserting
  `depth1_proved > 0 && depth2_proved > 0`.
- `a_real_aomenc_stream_with_filter_intra_on_a_sub16_horz_vert_strip_decodes_pixel_exact`
  -- **un-ignored**, GREEN (ignored since lane-fistrip r1).
- `a_real_aomenc_10bit_filter_intra_on_a_sub16_strip_decodes_pixel_exact`
  -- **un-ignored**, GREEN (his films are 10-bit; this is the 10-bit arm).

Both `#[ignore]` attributes and the stale prose in their doc comments are gone
in the same commit. No refusal string changed this round (r2 already deleted
the two `tx_w != tx_h` ones); `refusal_inventory` and `gate_coverage` stay
green in the suite below.

## Suite

`systemd-run --user --unit=rectsplitx-suite-r3-1788314510 -p MemoryMax=10G` ->
`$HOME/.cache/rectsplitx-suite-r3.log`: `test result: ok. 335 passed; 0 failed; 29 ignored; 0 measured; 0 filtered out; finished in 398.99s` (r2 was 1 failed / 31 ignored).

## Films (this worktree, not rebased -- other lanes' refusals dominate)

- `hg-head.obu`: `filter intra on a HORZ/VERT strip (this decoder predicts square-only)` (unchanged)
- `troy-head.obu`: `an intra block in an inter frame whose tx_depth splits its luma transform (round 1)` (unchanged)
- `hg5.obu`: `a HORZ_A/HORZ_B/VERT_A partition below 16x16` (unchanged; lifted on main by lane-ab16)

## Open

- deferred: rebase onto main `11633d7` -- this lane is 3 merges behind
  (`ce05d5f..11633d7`, palette2/ab16/tiles all touch `decode.rs` hot spots incl.
  `Reach::of_tu` arity and a duplicate `FIMODE_TO_INTRADIR` helper per the
  ledger). Unblocked by: whoever merges this lane resolving those conflicts;
  the fix itself is one field in a struct literal and applies unchanged.
- deferred: `troy-head.obu`'s refusal, "an intra block in an inter frame whose
  tx_depth splits its luma transform (round 1)", is this lane's family (the
  same split walk, in an inter frame). Unblocked by: an inter-frame gate with
  tx-size-search on.
- accepted: the coarse-cell `smooth_uv_neighbour(r * (SUB / MI), c * (SUB / MI), ..)`
  call form still stands at `decode.rs:5206`, `6197`, `7487`, `15310`. Those
  callers are 16px-aligned strips (32x16/64x32) where the SUB cell's mi IS the
  strip's mi, so they are correct today -- but a future sub-cell caller would
  repeat r2's defect.
