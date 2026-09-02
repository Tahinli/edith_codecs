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

## Round 3 (05b20f9)

### Fixed: the partition-context bug the round-2 finding pointed at
libaom's `update_ext_partition_context` (`av1_common_int.h:1500`) only
writes anything when `bsize >= BLOCK_8X8`; the four recursive
`decode_partition(BLOCK_4X4)` calls a real `PARTITION_SPLIT` at 8x8
produces are no-ops for context purposes. The *one* write that happens is
at the 8x8 level itself, with `subsize = BLOCK_4X4`:
`partition_context_lookup[BLOCK_4X4] = {31, 31}` (bit0 = 1), vs
`partition_context_lookup[BLOCK_8X8] = {30, 30}` (bit0 = 0) for a plain
`PARTITION_NONE` 8x8. `decode_leaf_split4`'s chroma-tail (the block that
sets `left_side_mi`/`above_side_mi` for the whole 8x8 group, `decode.rs`
~5350-5354) had copied `decode_leaf8`'s own TX4-branch value (`8`,
correct there -- that block is still one whole `BLOCK_8X8`, just with a
split *transform*) instead of the value that makes
`partition_ctx_mi`'s `left_side_mi[mi_r]*2 <= side` bit read as 1, which
needs `<= 4`. Changed both writes to `4`. Verified directly against the
libaom source at `~/.cache/aom-oracle/src/av1/common/av1_common_int.h:1441-1552`
(`update_partition_context`/`update_ext_partition_context`/
`partition_plane_context`), not by re-deriving it by hand.

**Effect measured**: before the fix, every attempt that hit a real SPLIT
crashed the very next sibling 8x8's `partition_w8` read (round 2's
finding). After the fix, `EC_AV1_TRACE=1` on the dumped stream that
previously crashed shows the sibling reads land on the *correct* context
(cross-checked bit-for-bit against instrumented aomdec's `EC_PART` trace
at the equivalent block, ctx encodes the same left/above bits both sides)
and the whole stream decodes to completion with `Ok`, no error, no panic.
This is real, but not sufficient -- see below.

### Range-ladder result: gate still RED, and the divergence is UPSTREAM of any sub8 code
Ran the decisive check the charter asked for: rebuilt
`~/.cache/aom-oracle` (rungs 2/5/10 already present in the source,
`EC_PART`/`EC_PART_VAL`/`EC_ISTEP` per-symbol range ladder), decoded the
one dumped failing stream (seed=139, cq=12, 64x64,
`--enable-rect-partitions=0 --min-partition-size=4 --max-partition-size=8`)
through instrumented `aomdec` and compared trace order against our own
`EC_AV1_TRACE` output block by block.

**First divergence**: the 16x16-level partition read at SUB-unit
`at16=(0,1)` (mi=(0,4)), the *second* 16x16 quadrant of the first 32x32
block -- i.e. it fires before our decoder has executed one single line of
`decode_leaf_split4`/`read_intra_mode_sub8` (the actual sub8x8-SPLIT in
this stream is at mi=(2,6), inside quadrant (0,1)'s own subtree, per
aomdec's `EC_PART_VAL mi_row=2 mi_col=6 bsize=3 value=3`). aomdec reads
`PARTITION_SPLIT` (value=3) there; ours reads `PARTITION_NONE` (value=0).
The context bits agree (both decode to left=1, above=0, aomdec's combined
`ctx=6` and ours `ctx=2` are the same left/above pair under different
context-array encodings) -- so this is not a context bug, it is either a
CDF-adaptation drift carried in from the *first* 16x16 quadrant's own
four 8x8 leaves (all plain `decode_leaf8`, zero sub8 code touched) or a
genuine range/probability desync from something even earlier that this
round did not get to trace symbol-by-symbol (no per-symbol trace exists
in our decoder for `decode_leaf8`'s own mode-info reads, only for the
`read_intra_mode_sub8` leaves added in round 1 -- that instrumentation
gap is why the ladder stops here this round).

**This changes the round-2 hypothesis**: the failure is very likely NOT
inside `decode_leaf_split4`/`read_intra_mode_sub8` at all. It reproduces
on this stream before any of that code has run. Whatever the true root
cause is, it sits in code shared by every 8x8/16x16 intra block in this
recipe (`decode_leaf8`, `partition_w16`/`partition_w32` context, or CDF
adaptation for `kf_y_mode`/`uv_mode`/`skip`), and `--min-partition-size=4`
is what surfaces it (this exact recipe may not be exercised by any other
gate). Round 4 should NOT keep assuming the bug is sub8-owned code --
start by adding the same per-symbol trace (`skip`/`mode`/`uv_mode`/
`angle_delta`) to `decode_leaf8` itself and ladder the *first* 16x16
quadrant's four 8x8 leaves symbol-by-symbol against aomdec's `EC_ISTEP`
trace before looking at sub8 code again.

### Suite / build state at HEAD (05b20f9)
- `cargo check --workspace --all-targets`: clean.
- Gate `a_real_aomenc_stream_with_a_sub8_split_decodes_pixel_exact`:
  still FAILS, now for a different, better-localized reason (see above).
  Only 1/40 attempts this round both fired a real SPLIT and reached the
  pixel comparison at all (39/40 either fired no SPLIT -- most now decode
  clean with `PARTITION_NONE` chosen throughout the min-partition-size=4
  region, itself now consistent with correct decoding since they don't
  panic -- or the run's own aomenc/decode plumbing skipped). The one that
  did fire failed pixel-exact per the finding above.
- `cargo test -p ec-av1 --lib`: not completed this round (turn budget
  spent on the range-ladder trace above); deferred to round 4, run it
  first thing.
- Debug-only instrumentation (`EC_AV1_DUMP_SUB8` stream dump in
  `stream.rs`, temp `dec.rng()` trace print attempt in `decode.rs`) was
  added, used, and **reverted** this round -- not left in the tree.

## Open (round 4 charter should start here)
- fix-now candidate: root cause is upstream of sub8's own code (see
  "changes the round-2 hypothesis" above) -- add per-symbol trace to
  `decode_leaf8` and ladder against aomdec `EC_ISTEP` on the first 16x16
  quadrant before touching `decode_leaf_split4` again.
- `cargo test -p ec-av1 --lib` full run: not done this round, do it first.
- Hunger Games extract probe (charter step 4): not reached this round --
  the gate is still red and the 10-bit wall in this branch (main merged
  10-bit at 5636fd2, this branch has not) makes it low-value until the
  above is fixed.

## Open (carried from round 2, unchanged)
- fix-now candidate: the desync in `decode_leaf_split4`/
  `read_intra_mode_sub8` (see "the actual finding" above) -- root cause,
  not yet found.
- deferred: tiny-frame (exactly-16x16) edge-straddle defect, unrelated to
  sub8, found as a side effect of workaround #2 above -- needs its own
  charter, not this lane's.
- deferred: Hunger Games extract probe (charter step 4) and the ledger
  append for round 1's findings (charter step 5) -- partially done here
  (this file), full ledger append below.
