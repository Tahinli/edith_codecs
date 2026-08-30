# lane-sbpart r7 — read the ladder

At 137f09c. **I merged main for you.** r6 merged, built oracle rung 10 and was
writing its report when it hit the cap; its tree is committed verbatim at
2c27c00, **unverified**. Check it compiles, then continue.

Read `lanes/sbpart-r5.report.md` and whatever r6 left in `lanes/`. The finding
that matters: the rect64 mismatch is an **entropy desync, not reconstruction**.
Block1 of the SB-level `PARTITION_VERT` pair is pixel-exact; block2 is wrong
from its first pixel; everything after it in raster order is wrong; everything
before it is exact.

Use the rung to range-ladder block2's first reads — skip, mode, angle_delta_y,
uv_mode. Compare msac RANGE, never `tell()` ([[compare-range-not-tell]]), and
read the result strictly: reference range unchanged where ours moves = we read a
symbol it never wrote; theirs moves and ours does not = we skipped one; only both
moving differently implicates a table ([[equal-range-means-unread]]).

r5 ruled out the CDEF per-SB guard by experiment. Its clearing of
`fill_skip_grid_rect`/`skip_txfm_ctx`/`record_rect` was **by inspection only** —
let the ladder decide.

## Budget discipline
75 turns, no reset on resume. At about turn 55, stop starting new work: commit
what is green and write your report. The merge is already done for you.

Hard rules: `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-sbpart`; foreground
`nice -n 19 cargo ... -j4`; every `cargo test` a timeout >= 600000 ms;
`EC_AV1_REQUIRE_AOMENC=1`; aomenc `--threads=1 --row-mt=0 --sb-size=64` and this
gate needs `--enable-tx-size-search=0`. The oracle is SHARED — env-gated rungs
only, never a throwaway patch left in the tree. Sibling worktrees have live
agents — never build in or edit them. Never push, never merge into main. End with
`lanes/sbpart-r7.report.md`, VERDICT on line 1.
