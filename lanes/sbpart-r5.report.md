VERDICT: PARTIAL -- answered the charter's headline question (ENTROPY DESYNC,
not reconstruction) with decisive evidence; did not fix the bug and did not
attempt the main merge (budget ran out at the diagnostic step; merge deferred
rather than rushed, per r2/r3/r4's own history of dying mid-edit).

## The answer: ENTROPY DESYNC, not reconstruction

Reproduced r2's pinned mismatch (`EC_SBPART_GATE_ATTEMPTS=1`, seed 42, dumped
via `EC_AV1_GATE_DUMP` to
`/tmp/.../scratchpad/sbpart-pin.obu`) and added a bounding-box + per-64px
superblock-column mismatch breakdown (env-gated `EC_SBPART_DIAG`, on the
`pinned_sbpart_stream_decodes_pixel_exact` test r4 left uncommitted-verified;
committed this round at dff47f8, diagnostics only, no behavior change to the
live gate).

Findings, decisive in order:
1. With the gate's own default flags (loop filters on), luma mismatches cover
   74% of the whole 192x128 frame (18291/24576 px), bounding box the entire
   image -- already suspicious for a "reconstruction only" bug, which should
   stay confined to the block(s) actually decoded by the new code.
2. Re-encoded the identical seed/recipe with `--enable-cdef=0
   --loopfilter-control=0` added (temporarily, reverted before commit -- the
   live gate is unchanged) to rule out loop-filter propagation muddying the
   bounding box. Result, per 64px superblock column (`TRACE_RECT64_END`
   confirms the SB-level VERT split fires at mi_row=0, mi_col=16/24, i.e. the
   second SB column, two 32x64 halves):
   - SB col 0 (px 0-63, decoded first, plain SPLIT/32x32 path, never touches
     `decode_block_rect64`): **zero mismatches**. Pixel-exact.
   - SB col 1 (px 64-127, the rect64 SB): the **first** `decode_block_rect64`
     call (px 64-95, block1) is **pixel-exact**; the **second** call (px
     96-127, block2) mismatches from its very first pixel (row 0, col 96).
   - SB col 2 (px 128-191, decoded right after the rect64 SB, plain
     SPLIT/32x32 path again): 8160/8192 px wrong, first mismatch at its very
     first pixel (row 0, col 128).
   - SB row 1 (below, decoded last): also garbage from its first pixel.
3. This is the textbook entropy-desync signature: a reconstruction bug (wrong
   stride, transposed scan table, axis-swapped embed) stays inside the
   spatial footprint of the block it corrupts. Here, everything decoded
   **after** block2 -- SB col 2, all of SB row 1, none of which ever calls
   `decode_block_rect64` or any rect-related code -- is also wrong, and
   everything decoded **before** it (SB col 0, and block1 itself) is
   perfect. The corruption starts exactly at block2's first symbol read and
   never recovers: the bitstream desynced between the end of the first
   `decode_block_rect64` call and the start of the second one, inside the
   `PARTITION_VERT` (and by construction, symmetrically `PARTITION_HORZ`) arm
   of the SB-level match at `decode.rs:5611-5643`.
4. This **overturns r2's suspects (a)/(b)** (corner-embed stride vs
   `dequant_and_inverse_typed_wh`'s w/h ordering; `default_scan(TX32)` vs
   `scan32`) -- those are both purely within-block reconstruction/coefficient
   paths and cannot explain corruption of spatially unrelated blocks decoded
   afterward. They are not ruled out as *additional* bugs, but they are not
   *this* bug.

## What is NOT the cause (checked and ruled out this round)
- `maybe_read_cdef_idx`'s `CDEF_TRANSMITTED` guard (`decode.rs:115-134`,
  reset per-SB at `decode.rs:5188`): correctly gates a second `cdef_idx`
  read within one SB (matches the existing, presumably-working
  `decode_block_rect` 32x32-level precedent at `decode.rs:2903`). Confirmed
  moot for the observed bug directly -- disabling CDEF entirely
  (`--enable-cdef=0`) reproduces the identical symptom (block1 clean, block2
  wrong from pixel 0), so this is not it.
- `fill_skip_grid_rect`/`skip_txfm_ctx`/`skip_at` (decode.rs:1610-1648):
  read/write spans for block1 (mi_c 16..24) correctly cover block2's true
  left-neighbour cell (mi_c 23); code-audited, looks right.
- `record_rect` (decode.rs:1912-1933): block1's `above_mode`/`left_mode`
  writes span `c..c+w/SUB` / `r..r+h/SUB` -- for block1 (bw=32,bh=64) that is
  `above_mode[4..6]`, `left_mode[0..4]`; block2 reads `left_mode[at.0]` =
  `left_mode[0]`, which block1 did write. Also looks right on inspection.

## Not yet found (next round's first move)
The exact wrong read is still unlocated -- code audit of every neighbour/
context computation touched by block2's first three reads (`skip`,
`mode`/`above_ctx`/`left_ctx` via `INTRA_MODE_CTX`, `angle_delta_y`) turned up
nothing obviously wrong by inspection; the previous two rounds' turn-cap
deaths were from chasing exactly this kind of manual trace without a ground
truth to compare against. **The next round should stop code-auditing and get
a real range/bit-position oracle**: an `aomdec` build patched to
`EC_TRACE`-print the same (mi_row, mi_col, symbol name, msac range) tuples our
decoder already prints via `EC_AV1_TRACE`, then range-ladder block2's first
handful of reads against it (per [[compare-range-not-tell]] and
[[price-the-narrowing-not-the-table]]) -- oracle rung 9 was reserved for this
in the charter and was not spent this round; do not add a second throwaway
patch, env-gate it in `scripts/build-aom-oracle.sh` the way rungs 6-8b did.

## What was committed (dff47f8)
Diagnostics only, no behavior change to the live gate
(`a_real_aomenc_stream_with_a_superblock_level_horz_vert_partition_decodes_pixel_exact`
is byte-identical to before this round): a `TRACE_RECT32_END` print in
`decode_block_rect` mirroring the existing `TRACE_RECT64_END`, and an
`EC_SBPART_DIAG` env-gated bounding-box/per-SB-column mismatch breakdown on
`pinned_sbpart_stream_decodes_pixel_exact` (r4's pinned-replay test, itself
already committed at 2f51dc1). `cargo check -p ec-av1`: clean.

## Merge NOT attempted this round
Ran out of turn budget reaching the bisect answer (the diagnostic loop above
took most of this round's calls -- large `cargo test` outputs on this box are
expensive to inspect). Per the charter's own budget discipline ("at turn 55
stop starting new work"), did not start the `main` (913df61) merge -- three
prior merges in this batch were text-clean/compile-broken, and starting one
with no turns left to fix a broken decoder-signature collision would be worse
than leaving it for a round with full budget. **This is the single biggest
item owed to the next round**, ahead of the bisect continuation above.

## Hard rules followed
Worked only in this worktree; `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-sbpart`
every build; foreground `nice -n 19 cargo ... -j4`; `EC_AV1_REQUIRE_AOMENC=1`
on every aomenc-driven test run; aomenc always `--threads=1 --row-mt=0
--sb-size=64`; no push, no merge into main, no other worktree touched; no
throwaway oracle patch left in the tree (rung 9 reserved but not spent).

## Next round, in order
1. Merge `main` (913df61) into `lane-sbpart`, compile immediately, fix any
   decoder-signature collision before doing anything else.
2. Build the `EC_TRACE`-patched `aomdec` oracle rung (env-gated, rung 9) and
   range-ladder block2's first ~5 symbol reads (`skip`, `mode`,
   `angle_delta_y`, `uv_mode`, `angle_delta_uv`) against it to name the exact
   wrong read.
3. Fix, then re-run the full-`n_attempts=40` gate and hard-assert
   `sb_rect_hits() > 0` (already written that way in the gate).
