# lane-sbpart r6 — range-ladder block2

At f68fe19. Read `lanes/sbpart-r5.report.md`. r5 delivered the answer three
rounds had failed to: the rect64 mismatch is an **entropy desync, not
reconstruction**. Block1 of the SB-level `PARTITION_VERT` pair
(`decode.rs:5611-5643`) decodes pixel-exact; block2 is wrong from its first
pixel; everything after it in raster order is wrong; everything before it,
including SB column 0, is exact. That overturns r2's stride and scan-table
suspects — drop them.

## Order
1. **Merge `main` first** (now 170a5a3 — loop restoration filters, tile-group
   OBUs, non-uniform tile spacing, a reworded sub-16x16 partition refusal) and
   COMPILE IMMEDIATELY. Three merges in this batch were text-clean and
   compile-broken because a newer gate called a decoder whose signature a lane
   had changed. r5 owed this and ran out of budget.
2. **Build oracle rung 9**: an `EC_TRACE`-patched decode path in
   `scripts/instrument-aom-oracle.sh`, env-gated and silent when unset, that
   prints the range after each element for the SB containing block2. Follow the
   existing rungs' wrapper-around-impl shape; rebuild with
   `scripts/build-aom-oracle.sh`. Rungs 6, 7, 8, 8b are taken and 10 is spoken
   for by a sibling lane, so take 9.
3. **Range-ladder block2's first reads** (skip, mode, angle_delta_y, uv_mode)
   against it. Compare msac RANGE, never `tell()` — decoders' tell baselines
   differ by fixed constants ([[compare-range-not-tell]]). Read the result
   strictly: the reference's range unchanged where ours moves means we read a
   symbol it never wrote; theirs moving where ours does not means we skipped
   one; only when both move differently is a table implicated
   ([[equal-range-means-unread]]).
4. Fix, then get the gate pixel-exact, then merge-ready.

r5 ruled out by direct experiment: `maybe_read_cdef_idx`'s per-SB
`CDEF_TRANSMITTED` guard (`--enable-cdef=0` reproduces the identical symptom).
It also code-audited `fill_skip_grid_rect`/`skip_txfm_ctx`/`record_rect`'s
above/left span writes and found them correct **by inspection only** — which is
exactly the kind of proof this repo has watched fail, so let the ladder decide
rather than trusting that audit ([[shared-oracle-blindness]]).

## Budget discipline
75 turns, and they do NOT reset if you are resumed. At about turn 55, stop
starting new work: commit what is green and write your report.

Hard rules: `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-sbpart`; foreground
`nice -n 19 cargo ... -j4`; every `cargo test` a timeout >= 600000 ms;
`EC_AV1_REQUIRE_AOMENC=1`; aomenc `--threads=1 --row-mt=0 --sb-size=64`;
`--enable-tx-size-search=0` is required by this gate's recipe. The oracle is
SHARED with sibling lanes — env-gated rungs only, never a throwaway patch left in
the tree. Sibling worktrees have live agents — never build in or edit them. Never
push, never merge into main. End with `lanes/sbpart-r6.report.md`, VERDICT on
line 1.
