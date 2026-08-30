VERDICT: PARTIAL -- compiles, all inertness/regression gates green (227/0 full lib, AB/free-partition gates green, wedge gate green on retry after a confirmed flake), but the whole-frame IDENTITY-only refusal in stream.rs was NOT narrowed: doing so exposed a real pixel mismatch (seed 43, frame 1 luma) on the interintra-wedge gate, so it stays in place. Non-IDENTITY global motion still refuses by name at the stream level; the internal decode/mvstack wiring (steps 1-6) is complete and compiles, but is unproven end to end.

## What changed
- `crates/ec-av1/src/decode.rs`: single-ref path now computes `is_global_mv_block` (blockd.h:421-429 -- mode GLOBAL* AND model > TRANSLATION AND min(bw,bh)>=8px) and `gm_nontrans` (reconinter.h:420-425 -- model != TRANSLATION, IDENTITY counts) as two separate locals, right after `is_globalmv`/`mv` are resolved. `gm_nontrans` now gates the single-ref `resolve_interp_filter` suppress arg (was `is_globalmv`); `is_global_mv_block` now gates `motion_mode_eligible` (`&& !(is_global_mv_block && !force_integer_mv)`, implicit SIMPLE_TRANSLATION) and `MiInfo::is_global_mv0`.
- `decode_inter_block` gained a `global_motion: &[WarpParams; 7]` param, threaded to all 18 call sites in `decode_inter_frame_tile_with_cdfs` plus the standalone unit-test call site and the `decode_inter_frame_tile` (no-cdfs) wrapper (`[WarpParams::default(); 7]`).
- `decode_inter_frame_tile_with_cdfs` gained the same param, threaded from `stream.rs`'s `header.global_motion`.
- `decode_inter_block8` (leaf8) stays gm-inert by design: its `find_mv_stack_compound` call got `&crate::mvstack::NO_GM_MV`, its `assign_compound_mv` call got `((0,0),(0,0))` -- compile-only fixes, its own GLOBALMV refusal (decode.rs ~7466) is untouched and still guards this leaf.
- `mvstack.rs`'s own unit tests (`find_mv_stack_compound` call sites) got `&NO_GM_MV` too (compile fix, same shape).
- `stream.rs`'s whole-frame `global_motion != IDENTITY` refusal: attempted removal (step 8), reverted same round -- see Dead-end below. Net diff there is a doc-comment only.

## Gate ladder (in order, per charter)
a. `cargo test -p ec-av1 --release --lib pinned -- --ignored --nocapture`: 2 passed (golden7, warp), 4 failed -- all 4 are pre-existing env-only gaps unrelated to gm (golden3/golden4 read a hardcoded path into a DIFFERENT prior session's scratchpad UUID that doesn't exist on this box; `scratch_decode_pinned_stream_once`/`scratch_isolate_pinned_mismatch` require `EC_AV1_PIN` set manually, by design). No IDENTITY-gm pin moved.
b. `EC_AV1_REQUIRE_AOMENC=1 cargo test ... a_real_aomenc_stream_with_interintra_wedge -- --nocapture`: first run FAILED on `wii_hits==0` (12/40 matched, 0 wedge fires) -- reproduced the documented parallel-flake-is-attempt-selection class, NOT a gm regression (all 12 matched streams are IDENTITY-gm since the whole-frame refusal was still in place at that point). Rerun: `ok`, 30 other-capability refusals / 10 matches / wii_hits=7. Refusal count both runs: 27/40 and (unlogged, similar) named "global motion...not IDENTITY" -- within the charter's 25-29/40 baseline, i.e. unchanged (expected, since step 8 wasn't landed yet at that point).
c. AB + free-partition gates: both `ok` (`a_real_aomenc_stream_with_free_partitions_decodes_pixel_exact`, `a_real_aomenc_stream_with_ab_partitions_decodes_pixel_exact`), rect_partition_hits=18, extended_partition_hits=1, partab_hits=7/2 across the two runs.
d. `cargo test -p ec-av1 --release --lib -- --skip a_real_aomenc_stream_with_interintra_wedge`: 227 passed, 0 failed, 17 ignored, 1 filtered.

## Step 8 (narrowing the whole-frame refusal) -- attempted, reverted
Removed the `header.global_motion != IDENTITY` whole-frame refusal in `stream.rs` and reran the AB/free-partition/wedge gates with `EC_AV1_REQUIRE_AOMENC=1 EC_WEDGE_GATE_ATTEMPTS=20 EC_RECTGATE_ATTEMPTS=20`:
- AB partitions: `ok`, 17 named refusals / 3 matches, **zero** "global motion" refusals logged (confirms the narrowing itself worked mechanically).
- Free partitions: `ok`, similarly zero gm refusals.
- Interintra-wedge: **FAILED** -- `a_real_aomenc_stream_with_interintra_wedge_decodes_pixel_exact frame 1 luma vs ffmpeg (seed 43)` pixel mismatch, decode.rs:3821 assert.

This is a real defect somewhere in the single-ref/compound GLOBALMV wiring (steps 1-6), not proven safe to ship. Reverted the stream.rs removal (restored the refusal, kept everything else) rather than land a wrong decode per the charter's own instruction. Compiled clean after revert, committed (2e23ae3).

## Dead-ends / open
dead-end: lane-gm r2 step-8 -- narrowing the whole-frame `global_motion != IDENTITY` refusal in stream.rs decodes AB/free-partition gates clean (0 gm refusals, both green) but mismatches pixel-exact on `a_real_aomenc_stream_with_interintra_wedge_decodes_pixel_exact` seed 43 frame 1 luma (`EC_WEDGE_GATE_ATTEMPTS=20 EC_AV1_REQUIRE_AOMENC=1`) -- a real defect in the single-ref/compound GLOBALMV MV/interp/motion-mode wiring, not yet root-caused. Refusal reverted/restored.
open: next round should reproduce seed 43 with `EC_AV1_GATE_DUMP=<scratchpad>/gm-flake-1.obu` set on that same recipe (48/64px gradients+mandelbrot, `--cq-level=45 --cpu-used=0 --auto-alt-ref=1 --lag-in-frames=16 --enable-fwd-kf=0 --enable-order-hint=1 --enable-warped-motion=1 --enable-obmc=1 --enable-interintra-comp=1 --enable-interintra-wedge=1 ...` -- the full args list is in `stream.rs`'s `a_real_aomenc_stream_with_interintra_wedge_decodes_pixel_exact`), self-pin it, and range-ladder against aomdec `EC_TRACE` on the GLOBALMV blocks in frame 1 to find whether the desync is in `gm_get_motion_vector`'s per-model math, the mv-stack neighbour substitution, `is_global_mv_block`/`gm_nontrans` predicate boundary, or `build_gm_mv_table`'s per-block-position recompute.

## Files touched
- `crates/ec-av1/src/decode.rs` -- gm predicates, threading, leaf8 compile stubs
- `crates/ec-av1/src/stream.rs` -- `header.global_motion` threaded to the call; whole-frame refusal doc-comment updated (refusal itself unchanged, net)
- `crates/ec-av1/src/mvstack.rs` -- own test call sites' `NO_GM_MV` compile fix

Commits (branch lane-gm, worktree edith_codecs-gm): a53b294 (compiles), 208e89e (test compile fix), 2e23ae3 (step-8 attempt + revert, documented).
