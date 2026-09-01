# lane-gmaffine r1 — VERDICT: NOT DONE. Branch must NOT merge.

Item (1) (AFFINE global motion, previous builder, HEAD 44bd68c) re-verified GREEN.
Items (2) 8x8 WARPED_CAUSAL and (3) 8x8 GLOBALMV are IMPLEMENTED but UNGATED —
both new gates are RED, so by COMMON's own rule ("a refusal is lifted ONLY together
with a gate") this branch is not mergeable as it stands.

## Item (1) re-verified
`a_real_affine_global_motion_stream_decodes_pixel_exact` still passes on this branch.
EVIDENCE: cargo test -p ec-av1 --release --lib (whole-suite run below) | gate ran with
EC_AV1_REQUIRE_AOMENC=1 | listed in the 266 passing tests, not among the 4 failures.

## What the round actually found (the premise in the charter was too narrow)
`decode_inter_block8` (the 8x8 leaf) was not merely missing warp/GLOBALMV: it was
**unreachable for interior blocks at all** — `decode.rs` refused every interior 16x16
`PARTITION_SPLIT` ("an inter partition below 16x16 …"), so the leaf only ran for
frame-edge straddling blocks. Wiring it exposed four more gaps in that same function,
each of which had to be closed before either charter gate could even reach a leaf:
non-LAST single references, no `read_mb_interp_filter` at all (silent entropy desync on
every SWITCHABLE stream), the missing short OBMC masks, and non-DC chroma modes.

## Changed (all in the worktree, branch lane-gmaffine)
- `crates/ec-av1/src/decode.rs`
  - 8x8 leaf: `GLOBALMV` via `gm_get_motion_vector` (`GLOBALMV_HITS_8`), replacing the
    "GLOBALMV (round 3)" refusal.
  - 8x8 leaf: `WARPED_CAUSAL` = `find_samples` → `select_samples` → `find_projection` →
    `warp_affine` on all three planes (`WARP_HITS_8`), replacing the "an 8x8 leaf that
    coded WARPED_CAUSAL" refusal.
  - 8x8 leaf: `allow_warp`'s global-warp branch (`global_warp_params`, independent of
    motion_mode), `MiInfo::is_global_mv0`, motion-mode suppression for
    `is_global_mv_block`.
  - 8x8 leaf: mv stack now built with `find_mv_stack_with_sign_bias(..., &gm_table, ..)`
    so a missing candidate falls back to the block's own gm mv (gm r6's root cause,
    previously fixed only at the 16x16+ leaf).
  - 8x8 leaf: full `read_single_ref` tree + `ref_planes` — "a reference frame other than
    LAST_FRAME (round 2)" removed.
  - 8x8 leaf: `resolve_interp_filter` read in libaom's order (interintra, motion_mode,
    filter) and `mc::predict_with_filters`; corner-cut documented in-code (neighbour
    filter ctx is the enclosing 16x16's, ceiling = siblings that pick different filters).
  - interior 16x16 `PARTITION_SPLIT` → four 8x8 leaves (same loop the straddle branch
    ran); the refusal narrows to "an inter partition below 16x16 other than a clean split
    into four 8x8 leaves".
  - `obmc_mask`: libaom `obmc_mask_1`/`obmc_mask_2` ([64], [45,64]) — the old
    `unreachable!()` was a hard PANIC as soon as a real 8x8 leaf ran OBMC.
- `crates/ec-av1/src/stream.rs` — `run_8x8_leaf_motion_gate` + gates
  `a_real_globalmv_8x8_leaf_stream_decodes_pixel_exact`,
  `a_real_warped_causal_8x8_leaf_stream_decodes_pixel_exact` (mandelbrot rotate+stretch,
  `--min/--max-partition-size=8`, cq sweep 32/45/55, 8- and 10-bit, hard-assert on the
  8x8-specific counters, no SKIP path).
- `crates/ec-av1/src/refusal_inventory.rs` — 3 strings removed, 1 narrowed.

## Gate results (all real runs, EC_AV1_REQUIRE_AOMENC=1)
EVIDENCE: cargo test -p ec-av1 --release --lib -- a_real_globalmv_8x8_leaf a_real_warped_causal_8x8_leaf
| 6 aomenc encodes per gate (8/10-bit x cq 32/45/55) | RED, 0 firing attempts. Last blockers:
globalmv gate = "a partition below 8x8" (cq32/45) and "a non-DC chroma mode on an 8x8
inter-frame leaf" (cq55); warp gate = a desync panic in `switchable_interp` (leaf entropy
state still wrong somewhere, most likely the coarse per-leaf neighbour contexts).

EVIDENCE: cargo test -p ec-av1 --release --lib -- a_real_aomenc_stream_with_obmc_8x8 |
same command, seed 53 | the pre-existing 8x8 OBMC gate, vacuous (never fired) for three
rounds, NOW REACHES 8x8 LEAVES and gets frame 14 luma+U exact, V off by ~2 at a handful of
positions — a real residual 8x8-leaf chroma defect, previously invisible.

## Suite
`cargo test -p ec-av1 --release --lib` (EC_AV1_REQUIRE_AOMENC=1): **266 passed, 4 failed,
23 ignored** — failures were `refusal_inventory` (fixed after that run), `obmc_8x8`
(residual chroma defect above), and the two new RED gates. No previously-green gate
regressed.

## Residue
- fix-now (next round): the 8x8-leaf residual that keeps every 8x8 gate from going green —
  order of attack: (a) chroma OBMC/prediction at BLOCK_8X8 (obmc_8x8 gate is one V plane
  away and is the cheapest instrument), (b) per-leaf neighbour contexts (filter/ref/skip
  arrays are all the enclosing 16x16's, a documented corner-cut that now matters),
  (c) non-DC chroma mode + sub-8x8 partition refusals at the leaf.
- deferred: compound `GLOBAL_GLOBALMV` per-ref global warp — grep found NO refusal for it;
  `decode_inter_block`'s compound branch builds both taps translationally through
  `mc::predict_compound_intermediate` and never calls `warp_affine`, so a compound global
  block with a ROTZOOM/AFFINE model is SILENTLY WRONG today. Unblocked by: a compound
  warp path (warp into the i32 intermediate, libaom `av1_warp_plane` with `conv_params`),
  which is its own lane-sized port. NOT started this round.
- accepted/flag: `warp::warp_affine` hardcodes `const BD: i32 = 8;` — worth one round's
  check against a 10-bit warp stream (his films are yuv420p10le).
