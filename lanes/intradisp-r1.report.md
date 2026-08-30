# lane-intradisp r1 report

VERDICT: PASS (skip-only scope, accepted ceiling)

## Scope (as implemented in 15b588f, recovered from a killed builder's stash)

Intra `PARTITION_HORZ`/`PARTITION_VERT` (32x16 / 16x32 strips) now dispatch in the
key-frame `match part32` (`crates/ec-av1/src/decode.rs`), via
`decode_block_rect`/`read_intra_mode_rect`/`tx_size_context_rect`/`cfl_ac_q3_rect`
and `PlaneBuf::edges_rect`/`reconstruct_rect`; `Reach::of_rect` +
`top_right_rect`/`bottom_left_rect` + `HAS_TOP_RIGHT_RECT`/`HAS_BOTTOM_LEFT_RECT`
in `encode.rs` (transcribed from libaom `reconintra.c`
`has_tr_32x16`/`has_tr_16x32`/`has_bl_32x16`/`has_bl_16x32`); FILTER_INTRA CDF
widened 4->6 rows (`BLOCK_32X16=12756`, `BLOCK_16X32=14301`) in `cdf.rs` with
`Cdfs.filter_intra`'s type updated in `cdf_state.rs`.

Deliberately SKIP-ONLY: every symbol the spec requires is read bit-exactly
(skip, y_mode, angle_delta, uv_mode, cfl_alphas, filter_intra, tx_depth) so the
arithmetic decoder stays in sync, but pixels are reconstructed only when
`skip` is set. A non-skip strip refuses BY NAME — chroma on these bsizes gets
a genuinely rectangular `TX_16X8`/`TX_8X16` from `av1_get_max_uv_txsize` with
no depth escape, and `transform.rs` has no rectangular transform at all. A
`filter_intra` strip on these bsizes also refuses by name
(`predict_filter_intra` is square-only). This scope-down is ACCEPTED.

## Gate numbers

### Full lib suite
`cargo test -p ec-av1 --release --lib` (EC_AV1_REQUIRE_AOMENC=1):
**224 passed; 0 failed; 17 ignored** (74.16s). Matches the charter's required
224/0.

### Pinned tests
`cargo test -p ec-av1 --release --lib pinned -- --ignored`:
found 6 tests named `pinned`/`scratch...pinned` under `#[ignore]`, plus 2 more
`the_inverse_transform_reproduces_the_pinned_*` tests that are NOT ignored
(already included in the 224 above, both `ok`). Not 14 — the "14-pin list" in
the charter did not match anything greppable in this tree; reporting the
actual set found.

- `pinned_golden7_stream_decodes_pixel_exact` -- ok, pixel-exact
- `pinned_warp_stream_decodes_pixel_exact` -- ok, pixel-exact (replays 5
  `rect-flake-*`/pinned fixtures via `EC_AV1_GATE_DUMP_PIN`, all matched)
- `pinned_golden3_stream_decodes_pixel_exact` -- FAILED: `reading pinned
  stream: NotFound`. Pre-existing, NOT caused by this lane: the test's
  default path is a hardcoded scratchpad path from an unrelated prior
  session (`/tmp/.../51b5f611-.../scratchpad/golden3-pin.obu`), introduced in
  commit `bec2741` (loop-filter-level fix, unrelated to intra). It is
  documented in its own doc comment as "the file only exists on this
  machine's scratchpad... run manually" and gated behind
  `EC_AV1_GATE_DUMP_PIN` for exactly that reason. No regression.
- `pinned_golden4_stream_decodes_pixel_exact` -- FAILED, same cause/same
  commit, same disposition.
- `scratch_decode_pinned_stream_once` / `scratch_isolate_pinned_mismatch` --
  FAILED: `set EC_AV1_PIN to the .obu path: NotPresent`. These are ad-hoc
  scratch harnesses that only run when a developer sets `EC_AV1_PIN`
  manually; failing with no env var set is by design, not a gate.

None of the 4 "failures" above are regressions from this lane; all 4 are
pre-existing manual-only harnesses that require local machine state this
worktree does not have. The two rect-intra-relevant pin gates that ARE
self-contained (`pinned_golden7`, `pinned_warp`) both pass pixel-exact.

### Named intra gates (4/4 pass)
`a_real_aomenc_filter_intra_stream_decodes_pixel_exact` -- ok
`a_real_aomenc_intra_stream_with_tx_select_decodes_pixel_exact` -- ok
`a_real_aomenc_stream_with_directional_chroma_decodes_pixel_exact` -- ok
`a_real_aomenc_intra_stream_with_deblocking_decodes_pixel_exact` -- ok
(matched the charter's "intra_with_deblocking" by substring "intra" +
"deblocking"; the real test name is `a_real_aomenc_intra_stream_with_deblocking_decodes_pixel_exact`.)

### `a_real_aomenc_stream_with_free_partitions_decodes_pixel_exact`, 3 runs
(this gate is flaky run-to-run by construction -- aomenc RD attempt
selection changes which of 40 seeds land which partition/refusal; reporting
spread, not one number.)

| run | named refusals | pixel-exact / 40 | rect_partition_hits |
|-----|-----------------|-------------------|----------------------|
| 1   | 32              | 8                 | 4 |
| 2   | 35              | 5                 | 6 |
| 3   | 33              | 7                 | 7 |

Prior single-run reads on file: before-lane 32/8/40 (rect_hits=12),
after-lane 31/9/40 (rect_hits=10). This round's spread (32-35 refusals,
5-8 pixel-exact, rect_partition_hits 4-7) sits inside/around that prior
"after" point but does not show a clean, repeatable directional move --
consistent with the documented aomenc-attempt-selection flake class
(parallel-flake-is-attempt-selection), not a regression. The new named
refusal string is now visibly firing across all 3 runs:
`"a non-skip HORZ/VERT intra strip needs a rectangular transform this
decoder does not code yet"` -- confirms the skip-only HORZ/VERT dispatch is
reached by real aomenc-encoded content and correctly refuses by name rather
than desyncing, which is exactly the scope this lane targeted.

## Scope-down findings (accepted, not lifted this lane)

1. **Chroma rectangular TX is the ceiling.** `av1_get_max_uv_txsize` gives a
   genuine `TX_16X8`/`TX_8X16` for chroma under a 32x16/16x32 luma block with
   no depth escape to a square TX, and `crates/ec-av1/src/transform.rs` has
   no rectangular DCT/ADST at all. A follow-up lane needs real rectangular
   transform kernels plus rectangular scan-order/eob-context tables in
   `transform.rs` (same shape as the existing square-only implementation) to
   lift the non-skip refusal.
2. **`filter_intra` is square-only.** `predict_filter_intra` only handles
   square blocks; a `filter_intra`-flagged HORZ/VERT strip refuses by name
   for the same reason -- prediction, not just residual, needs a rectangular
   path.

Neither blocker was worked around; both are named refusals that keep the
arithmetic decoder in sync (all preceding/following symbols still read
bit-exactly) rather than silently mis-decoding.
