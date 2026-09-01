# lane-inter8 r1 -- inter partition tree: the 16x16 level and its 8x8 leaves

Branch `lane-inter8`, off main `3808cf8` (resumed from the orchestrator's WIP
snapshot `a5430ec`). **NOT merge-ready**: the round's gate is RED and `#[ignore]`d
with its reason (see "Open residue"). Nothing here has been merged to main.

## What changed

- `crates/ec-av1/src/decode.rs` (inter tile loop, ~12345): the interior (non
  edge-straddling) 16x16 partition symbol is now read as the real alphabet and
  `PARTITION_SPLIT` recurses into four 8x8 inter leaves (previously: refused by
  name). Refusal narrowed to
  `"an inter partition below 16x16 other than SPLIT (16x8/8x16 rect inter leaves are not coded yet)"`.
  Counter `INTER_SUB16_SPLIT_HITS` / `inter_sub16_split_hits()` counts *interior*
  splits only -- the straddle path always ran that same loop, so only an interior
  count proves the newly-lifted alphabet value fired. (from the WIP snapshot)
- `decode.rs` `obmc_blend`: `av1_skip_u4x4_pred_in_obmc` -- the ABOVE pass skips
  chroma when the chroma block is 4x4/8x4/4x8 (8x8, 16x8, 8x16 luma at 4:2:0);
  `obmc_mask_2` added. (from the WIP snapshot)
- `decode.rs` `decode_inter_block8` (8x8 leaf), THIS round, three real defects,
  each one moving the aomdec range ladder strictly later:
  1. the leaf built its `comp_mode` / compound-ref contexts from a FABRICATED
     neighbour (`ref0 = LAST_FRAME` if inter, `ref1 = None`, `uni = false`).
     `av1_get_reference_mode_context` asks whether a neighbour is COMPOUND, so
     with real LAST+GOLDEN compound neighbours the leaf picked the wrong CDF row
     and read single-ref where aomdec read compound `NEAR_NEWMV` (mi 4,14).
     Now uses `neighbours.above_ref/above_ref1/left_ref/left_ref1` with the same
     `is_uni_comp_ref` logic as `decode_inter_block` one level up.
  2. the sibling-leaf override was a single `prev_leaf` (the previously decoded
     leaf). The bottom-LEFT leaf's ABOVE neighbour is leaf 0, never leaf 1, so
     one of the four leaves always read a stale neighbour -- class
     `context-read-from-one-cell`. Now a `prev_leaves` slice, matched by
     coordinate, and each leaf hands back its own resolved `(ref0, ref1)`.
  3. both leaf mv stacks were built with `NO_SIGN_BIAS` / `NO_GM_MV` while the
     16x16 path passed the frame's real tables; `assign_compound_mv` got
     `((0,0),(0,0))` for the gm mvs. Now threaded (`sign_bias_table`,
     `global_motion` params + a leaf-local `build_gm_mv_table`). Alignment with
     the proven path; it did NOT move this fixture's ladder (identity gm here).
- `decode.rs`: `EC_MODE mi_row=.. mi_col=.. rng=..` traces under `EC_TRACE_MODE`
  in both `decode_inter_block` and `decode_inter_block8`, byte-format-identical
  to the oracle's rung-4 `EC_MODE`, plus `EC_LEAFMODE` (compound mode / stack
  size / ctxs) and a `TRACE partition_w16` line under `EC_AV1_TRACE`. These are
  the instruments the round was actually decided with.
- `crates/ec-av1/src/stream.rs`: `inter_sb_none_gate` generalised to take
  `min_part` / `max_part` (aomenc `--min/--max-partition-size`) and the counter
  + label that proves the arm under test fired, so one recipe can target any
  level of the inter partition tree. Two new gates
  (`a_real_aomenc_[10bit_]inter_sequence_with_an_8x8_leaf_split_decodes_pixel_exact`,
  `min=8 max=16`) assert `inter_sub16_split_hits` moved and every frame is
  pixel-exact -- currently `#[ignore]`d, RED.

## Gate + evidence

Command (from the worktree, `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-inter8`,
`EC_AV1_REQUIRE_AOMENC=1`):

    cargo test -p ec-av1 --lib -- --ignored an_8x8_leaf_split

EVIDENCE: /tmp/a_real_aomenc_inter_sequence_with_an_8x8_leaf_split_decodes_pixel_exact-refused.obu | aomenc mandelbrot 64x64 4 frames, cq40 cpu0 --min-partition-size=8 --max-partition-size=16, rect/AB/1to4 off; decoded by ours (EC_TRACE_MODE) and by the instrumented aomdec (EC_TRACE_MODE) | partition symbol stream agrees 118/118 (size,ctx,value) vs aomdec EC_PART; inter mode-info range ladder agreed 41/57 before this round's fixes and 42/57 after, first divergence moved from block mi=(4,14) to mi=(6,12)
EVIDENCE: /tmp/ours_mode4.txt vs /tmp/orc_mode.txt | same stream, EC_MODE ladders diffed line for line | ours "EC_MODE mi_row=6 mi_col=14 rng=33349" vs oracle "rng=36254" -- entropy divergence inside block (6,12); before the fixes the stream refused outright ("a partition below 8x8"), it now decodes 4 frames without a refusal but is NOT pixel-exact

## Refusals

- narrowed: "an inter partition below 16x16 (8x8 and smaller inter blocks are not
  coded yet)" -> "... other than SPLIT (16x8/8x16 rect inter leaves are not coded
  yet)". **This narrowing is NOT yet backed by a green gate** -- honest status,
  and the reason this branch must not merge as-is.
- untouched: SB-level non-NONE/SPLIT, the 32x32 alphabet refusal (`value=9`,
  `PARTITION_VERT_4`, which aomenc emitted at 32x32 even with
  `--enable-1to4-partitions=0` -- same shape as lane-sbpart r2's AB-at-64 note),
  the 16x16 true-edge rectangular-transform one, the 8x8 chroma-mode one.

## Open residue

- fix-now (next round): `Neighbours`' `above_*`/`left_*` arrays are 16x16
  granular. Leaf (6,12)'s LEFT neighbour must be leaf (6,10) of the previous
  16x16 block, but both leaf rows of a 16x16 share one array slot, so one of
  them is always wrong. Making those arrays mi-granular (or adding a per-mi
  side band for skip/inter/ref0/ref1) is the next slice, and it is what
  un-ignores the two gates above.
- deferred(the above): 8x8 HORZ/VERT -> 8x4/4x8 leaves, rect inter MC/transforms
  at 16x8/8x16 (lane-rectx owns the transform half), AB/1-to-4 at 64/32/16, and
  the aggregate per-type-per-level hit-counter gate the charter asks for. None
  of these were started this round.
- deferred(unknown): the 10-bit variant of the same recipe stopped at
  "a reference frame other than LAST_FRAME (round 2)" at `--min-partition-size=8
  --max-partition-size=64` before the max was clamped to 16; not re-measured.
