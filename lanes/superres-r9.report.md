VERDICT: OPEN -- r8's derived scaled-MC algorithm is implemented and pixel-pinned at
REF_NO_SCALE, `decode_inter_block`'s 19 call sites all thread `frame_width`, the single-ref
branch scaled-vs-unscaled fork is wired, and the three unimplemented combinations (compound,
warp/OBMC/interintra, `decode_inter_block8`'s 8x8-leaf split) refuse by name -- but no bypass
gate was built this round to drive the new code with a real scaled reference, so `stream.rs:217`'s
frame-level refusal stays in place and no capability claim is made. Suite green throughout
(scoped runs below); turn budget spent on the algorithm + the wide 19-site threading, none left
to safely build+verify the bypass gate.

## What was done

1. `git merge 913df61` into `lane-superres` -- clean (19 files, loop restoration filters +
   `lr-r2..r9` lane docs). `cargo check -p ec-av1`: clean, no signature breakage. Commit `08e12c4`.
2. `mc::predict_scaled`/`mc::scale_factor`/`mc::REF_NO_SCALE` added to `mc.rs`, per r8's
   pseudocode verbatim (`x_scale_fp`/`x_step_qn`/`pos_x_q10`/per-column `x_qn` walk; vertical
   pass copy-pasted from `predict_with_filters`, untouched per r8's "AV1 superres never scales
   height" proof). New test `predict_scaled_at_no_scale_matches_predict_with_filters`
   (`mc.rs`) asserts `REF_NO_SCALE` reproduces `predict_with_filters` byte-for-byte across
   block widths 4/8/16 and every 1/16-pel fraction -- passes (8/8 `mc::tests`/`mc::` green).
   Commit `7f605df`.
3. `decode_inter_block` gained `frame_width: usize` as its final parameter. All 19 call sites
   updated: 18 real sites in `decode_inter_frame_tile_with_cdfs` (each already ending
   `allow_screen_content_tools,`, `frame_width: u32` already in that function's scope --
   scripted insert of `frame_width as usize,` after that line at each site) plus the 19th, the
   `decode.rs` unit-test helper at (then) line 12632 whose arg list ends `false,` -- hand-patched
   with `width` (the test's own local frame width). Commit `7f605df`.
4. Scoped refusals + the scaled-vs-unscaled fork, per the report's own line numbers (shifted by
   the merge, located fresh):
   - Compound branch (`resolve_interp_filter` return, before the two
     `predict_compound_intermediate` calls): refuses `"a compound-reference block with a scaled
     reference"` when either tap's `py0.width`/`py1.width` != `frame_width` --
     `predict_compound_intermediate` has no scaled counterpart.
   - Single-ref branch (right before `pred_y`/`pred_u`/`pred_v` are built, after
     `warp_params`/`obmc_selected`/`interintra_mode` are all resolved): computes
     `luma_scale = mc::scale_factor(py_ref.width, frame_width)` once; refuses `"warp/OBMC/
     interintra prediction with a scaled reference"` when `luma_scale != REF_NO_SCALE` and any
     of those three fired; otherwise branches `predict_with_filters` (unchanged call, byte-exact
     for the unscaled case per the pin above) vs `predict_scaled` per luma and both chroma planes,
     the same `luma_scale` reused unchanged for chroma per r8's "luma-derived ratio applies as-is
     to chroma" derivation.
   - `decode_inter_block8` (a separate function, one call site, NOT in the charter's 19 --
     its own compound/single-ref MC calls are un-threaded this round): refuses `"an 8x8 partition
     leaf under a scaled reference"` up front, before the leaf loop, when `ref_y.width` or any
     live `ref_slots` entry's width differs from `frame_width` -- a coarse over-refusal (blocks
     the whole 16x16-split-to-8x8 shape under ANY scaled reference, not just the leaf that
     actually picks one) rather than threading `frame_width` through its own body, given the
     turn budget.
   Commit `065a0b8`.
5. `EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 -j4 superres -- --test-threads=1`: 6/6 passed
   (`a_real_aomenc_stream_with_superres_refuses_by_name`,
   `a_real_aomenc_superres_key_frame_sequence_decodes_pixel_exact`, and the 4
   `superres::tests::upscale_row_*` reconstruction-filter pins) -- no regression from the new
   parameter/branches. `cargo test -p ec-av1 -j4 mc::`: 8/8 passed.

## Why the bypass gate (charter step 1 / this round's nominal first task) is not built

Read in the order the charter gave, but built in dependency order instead: the gate the charter
wants (decode a real aomenc key frame with `use_superres`, feed its picture as `reference` into
`decode_inter_frame_tile_with_cdfs` directly for a `--superres-denominator=12` inter frame,
bypassing `decode_stream`'s refusal) needs the scaled-MC code path to exist and compile first to
be worth driving -- writing it against not-yet-existing `mc::predict_scaled`/`frame_width`
plumbing would have meant rewriting the gate mid-round anyway. With the plumbing now landed and
compiling, the gate itself (mirroring
`a_real_aomenc_stream_with_two_tile_rows_decodes_pixel_exact`'s pattern per r8's own step 5) is
the very next, well-specified task -- but was not started this round: the 19-site threading plus
three refusal sites, each needing the surrounding branch re-read fresh (line numbers shifted by
the merge), spent the turns this round's checkpoint allows before a gate could be built and
debugged safely.

## Disposition

- deferred: the bypass gate itself (charter's own step 1, r8's step 5) -- next round, no further
  libaom reading needed; the exact recipe (`--superres-denominator=12 --kf-max-dist=1000`,
  disable warp/obmc/compound/interintra the same way the key-frame superres gate already does)
  is unchanged from r8's report. Once pixel-exact, lift `stream.rs:217`'s refusal (scoped to the
  combinations that still refuse, not deleted outright) and update `refusal_inventory.rs` in the
  same commit, per the charter.
- deferred: `decode_inter_block8`'s own scaled-MC threading -- currently a blanket refusal
  instead of the narrow per-leaf one `decode_inter_block`'s sites get; upgrade ceiling is
  threading `frame_width` through its ~15-param list the same mechanical way, once the primary
  bypass gate is green and a real stream is found that actually reaches an 8x8-leaf split under
  `use_superres` (unconfirmed whether the corpus even produces one).
- fix-now: none -- no defect found this round, only new (refused-by-default) capability.

## Refusal strings

Added: `"a compound-reference block with a scaled reference (superres, unimplemented)"`,
`"warp/OBMC/interintra prediction with a scaled reference (superres, unimplemented)"`,
`"an 8x8 partition leaf under a scaled reference (superres, unimplemented)"` -- all newly
reachable code paths (the single-ref non-warp/OBMC/interintra scaled branch is the only new
non-refusing path, and it is not yet reachable from `decode_stream`: `stream.rs:217`'s
frame-level refusal still fires first for every non-key `use_superres` frame).
`refusal_inventory.rs`'s own self-check (`the_decode_path_refuses_exactly_the_listed_cases`)
caught all three as undeclared on first run -- added to `REFUSALS`, now green. `stream.rs:217`'s
frame-level refusal itself (`"an inter frame with use_superres set..."`) is untouched -- no
refusal was LIFTED, so per [[refusal-lifted-without-a-gate]] no capability claim is made; the
inventory addition here is declaring new refusals, not removing the old one.

## Merge

Done, committed (`08e12c4`), suite green (scoped runs above). Satisfies the charter's "merge
main and compile immediately" step.
