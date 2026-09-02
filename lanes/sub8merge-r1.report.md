# lane-sub8merge report (round 1) -- merge cross-product defect closed

## Root cause (one line, one variable)

The merge resolution of `Reach::top_right` / `Reach::bottom_left`
(`crates/ec-av1/src/encode.rs:1237,1272`) introduced

```rust
let row = Self::table(side);          // 0..=3, the TABLE row
```

which SHADOWS the block's position row bound eight lines above
(`let (row, col, per_side) = Self::position(side, x, y);`) and is then used by
the bit index `(row * Self::table_stride(side) + col)`. Main computed the table
index inline (`HAS_TOP_RIGHT[Self::table(side)]`) and kept `row` = position.
So on the merged tree EVERY block size read `has_top_right`/`has_bottom_left`
at the wrong bit: the reach (above-right / below-left reference availability)
was wrong for ordinary square blocks, not just sub-8x8 -- which is exactly why
each side was green alone and the merged tree failed a gate whose stream
contains zero sub8 splits and zero split-tx strips (seed 55 printed
"split-tx strips this stream: 0").

Fix: rename to `table_row` in both functions, restoring `row` as the position
row, plus a NB comment naming the shadowing hazard.
`crates/ec-av1/src/encode.rs:1234-1241,1269-1276`.

Class: `adjacent same-typed args` / shadowing sibling -- a merge that renames a
value into an identifier already live in scope. Sweep: `grep -n "let row = \|let col = " crates/ec-av1/src/encode.rs`
returns nothing else; the only two `Self::position*` consumers are these two.

Also removed the previous agent's leftover ablation hook
`EC_ABLATE_SUB8` in `decode_leaf_split4` (`crates/ec-av1/src/decode.rs:6978`).

The charter's untested suspects (the six `smooth_neighbor_uv` sites, the
per-tile mi-map resets, `Neighbours::new`, `record_split_luma_rect`) are all
INNOCENT: they are unchanged by this round and both gates are green.

## Gates

EVIDENCE: $HOME/.cache/sub8merge-suite.log | `EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib split_transform_horz_vert` on the merged tree before and after the rename | before: FAILED, seed 55 first luma mismatch (191,76) ours=127 ffmpeg=128, 3080 samples, bbox x94..191 y76..127; after: ok, 1 passed (8.55 s)
EVIDENCE: same log | `EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib sub8` after the rename | ok, 1 passed (5.10 s, 3 tile arms x 4 firing+pixel-exact runs)
EVIDENCE: $HOME/.cache/sub8merge-suite.log | full `EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib` (1228 s) | 298 passed, 1 failed, 27 ignored -- every sibling gate named in the charter (sub8, tiny_frame_size_sweep, tx_select, an_8x8_leaf_split, split_transform_horz_vert, filter_intra_on_a_horz, superblock_level_*, refusal_inventory, gate_coverage) green

## The one failure is INHERITED FROM MAIN, not this lane

`decode::tests::nz_map_ctx_offset_tables_match_the_rect_rule` fails:
`32x64 nz_map offset at display (row 0, col 2): left 6, right 11`
(decode.rs:16837). `cdf.rs` is byte-identical between the merge base 61c6a5b,
this branch, and current main df5d630, and the test body is identical on
df5d630 (`git show df5d630:crates/ec-av1/src/decode.rs`, lines 16299-16321) --
so main itself is red on this test. `NZ_MAP_CTX_OFFSET_32X64` row 2 is
`[6, 6, 21, 21, 21]`, i.e. under the column-major read the table gives 6 where
the rule (w<h, display row<2) wants 11 for display rows 0/1 at columns 2/3.
Either the lane-rectsplit table or its rule test is wrong.
Disposition: **deferred(the rectsplit owner)** -- changing the table changes
real coefficient contexts and needs its own aomenc gate run; out of this merge
lane's scope and demonstrably not caused by it.

## Suite
`EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib`: 298 passed, 1 failed
(inherited, above), 27 ignored, 1228 s. Log: `$HOME/.cache/sub8merge-suite.log`.

## Refusals
None lifted or added this round; the merge's own bookkeeping (refusal string
narrowed to "a sub-8x8 leaf that uses intrabc" + inventory entry, gate_coverage
8-bit list dropping enable-1to4-partitions) came in with ea90092 and both
`refusal_inventory` and `gate_coverage` tests are green.
