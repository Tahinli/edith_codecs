# lane-sub8 report (rounds 1-2)

## Round 1 (2f35dec, a564705)
Implemented `read_intra_mode_sub8` + `decode_leaf_split4` in
`crates/ec-av1/src/decode.rs` (~5140-5370): decodes `PARTITION_SPLIT` of an
8x8 intra block into four `BLOCK_4X4` leaves (spec `decode_partition`'s
recursion bottom). Both intra call sites (the straddle path and the clean
16x16-SPLIT path) branch on the new `part8` read; `PARTITION_HORZ`/`VERT`
below 8x8 and all inter sub-8x8 stay refused by name.

**is_chroma_reference derivation** (spec/libaom `av1_common_int.h:1454`):
for 4:2:0 subsampling, only the *last* (bottom-right, index 3) of the four
4x4 leaves in a split 8x8 group carries chroma syntax at all -- the whole
8x8 group's one 4x4 chroma unit is read and reconstructed once, on that
leaf. `decode_leaf_split4` implements this via `has_chroma = i == 3`.

**Filter-intra CDF row**: `BLOCK_4X4`'s own row (`cdf.rs FILTER_INTRA[0] =
4621`) is already present and is what every sub-8x8 leaf here uses
(`filter_intra_size_class(4)` in libaom is class 0 for all sub-8x8 sizes).
If 4x8/8x4 (HORZ/VERT below 8x8) were ever implemented, they would need two
*more* rows: `BLOCK_4X8 = 6743`, `BLOCK_8X4 = 5893` (libaom
`BLOCK_SIZES_ALL` order, values from `entropymode.c:821`) -- not present
in this table yet.

**Why HORZ/VERT below 8x8 stays refused**: this decoder's transform
primitive is square-only. A real 4x8/8x4 leaf needs an actual rectangular
TX_4X8/TX_8X4 transform (scan order, EOB context, coefficient CDF set all
differ from the square 4x4 case); faking it as two 4x4 squares would desync
against a real encoder immediately. This is a scope cut, not a bug --
tracked by refusal name in `refusal_inventory.rs`.

## Round 2 (6703411)
Round 1 left the gate (`a_real_aomenc_stream_with_a_sub8_split_decodes_pixel_exact`,
`stream.rs` ~6604) uncompiled. It now compiles clean
(`cargo check -p ec-av1 --lib`, zero errors). **The gate is still RED.**

### What was tried
1. `--enable-rect-partitions=0` added to the aomenc recipe: necessary but
   not sufficient. Without it, aomenc picks a real `PARTITION_HORZ`/`VERT`
   at 8x8 (still refused by name) in ~63% of the original 40 attempts
   (25/40). With it, that specific refusal drops but the gate still hits
   0/160 total passing attempts across two fixture shapes (see below).
2. Shrinking the fixture to a single-16x16-leaf frame (16x16 pixels, so at
   most four 8x8 candidates exist in the whole frame) to structurally rule
   out a second, still-refused HORZ/VERT leaf sharing the tile. This
   surfaced an **unrelated, pre-existing tiny-frame defect**: a plain
   16x16 aomenc stream with NO `--min-partition-size`/`--max-partition-size`
   override and no sub8 code involved at all (`decode_leaf_split4` never
   called) still decodes with wrong pixels for most seeds (2/3 sampled).
   `EC_AV1_TRACE=1` shows the bitstream parses cleanly (no refusal, no
   panic, valid-looking symbol values throughout) but the picture desyncs
   silently partway through -- consistent with the frame-edge/"true edge
   straddle" logic (`decode.rs` ~6864-6957, the `has_cols16`/`has_rows16`
   branch) being exercised at a degenerate case (frame == exactly one
   16x16 block, most of the SB out of frame) that larger sub-SB frames
   (32x32, 64x64, used successfully elsewhere in this file) don't reach.
   **Out of scope for this lane** -- not touched, not fixed, flagged here
   for whichever lane owns edge-straddle handling next.
3. Widening the seed/cq sweep for the 64x64 fixture (with
   `--enable-rect-partitions=0`) to 160 total attempts across two batches:
   still 0 passes.

### The actual finding
`EC_AV1_TRACE=1` on failing 64x64 attempts shows the pattern directly:
`partition_w8 mi=(0,0) ... value=3` (a real, clean `PARTITION_SPLIT`,
`decode_leaf_split4` fires, `SUB8_SPLIT_HITS` increments) is immediately
followed by `partition_w8 mi=(0,2) ... value=2` (`PARTITION_VERT`) on the
very next sibling 8x8 leaf in the same 16x16 parent -- and that leaf hits
the (correct, by-design) HORZ/VERT-below-8x8 refusal. This happened on
every single attempt that reached a SPLIT at all, which is not consistent
with random encoder choice given `--enable-rect-partitions=0` is set (per
libaom `partition_search.c`'s `do_rectangular_split` gating, that flag
should make `PARTITION_HORZ`/`VERT` unreachable via RD at 8x8 too). The
more consistent read: `decode_leaf_split4`/`read_intra_mode_sub8` leaves
some piece of decoder, CDF, or neighbour-context state wrong on return, and
the very next `partition_w8` symbol read (on the sibling leaf) decodes a
value the real bitstream did not write -- i.e. a real desync introduced by
this round's own new code, immediately after it runs.

**This was not chased down further this round** -- turn budget spent
finding and narrowing it. Next round should NOT try more fixture-shape
workarounds; instead: trace `dec`'s range/tell before and after
`decode_leaf_split4` against a byte-identical aomdec run (mirror the
`av1-mvstack-refmv-corner`/`cdf-counter-not-reset` class method: exact
range ladder up to the read, compare the step) to find which of
`read_intra_mode_sub8`'s reads or which piece of `Neighbours`/CDF
bookkeeping in `decode_leaf_split4`'s tail (the chroma-neighbour section,
lines ~5346-5369) is wrong.

### Suite / build state at HEAD (6703411)
- `cargo check -p ec-av1 --lib`: clean, 0 errors.
- Gate `a_real_aomenc_stream_with_a_sub8_split_decodes_pixel_exact`: FAILS
  (0/40 in-suite attempts pass; `sub8_split_hits()` DOES go nonzero during
  the run -- the SPLIT path fires -- but no attempt completes pixel-exact).
- Full `cargo test -p ec-av1 --lib` and Hunger Games extract probe: not run
  this round (turn budget spent on the above investigation). Deferred to
  round 3.

## Open (round 3 charter should start here)
- fix-now candidate: the desync in `decode_leaf_split4`/
  `read_intra_mode_sub8` (see "the actual finding" above) -- root cause,
  not yet found.
- deferred: tiny-frame (exactly-16x16) edge-straddle defect, unrelated to
  sub8, found as a side effect of workaround #2 above -- needs its own
  charter, not this lane's.
- deferred: Hunger Games extract probe (charter step 4) and the ledger
  append for round 1's findings (charter step 5) -- partially done here
  (this file), full ledger append below.
