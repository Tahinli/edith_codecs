# lane-fimv r1 report

Branch `lane-fimv`, rebased onto main `df5d630`. Commits:

- `0fea969` (rebased `7513e53`) `fix(av1): motion_mode alphabet ignored force_integer_mv (and scaled refs)`
- `fc8b30e` `test(av1): force_integer_mv motion-mode gate asserts non-vacuity per attempt`

## Root cause (r1, first commit -- verified this round, not re-derived)

libaom `motion_mode_allowed` (`blockd.h` ~1484) returns `WARPED_CAUSAL` only when
`!xd->cur_frame_force_integer_mv && !av1_is_scaled(...)`. Our `warp_eligible`
(`crates/ec-av1/src/decode.rs:12417`) had neither clause, so a screen-content frame whose
header set `force_integer_mv` read the 3-symbol `motion_mode_cdf` where libaom wrote the
2-symbol `obmc_cdf` -- a wrong-alphabet msac narrowing, silently wrong pixels with no
refusal naming it (class `wrong-alphabet-same-value`). Same clause in
`decode_inter_block8`'s 8x8-leaf read (`decode.rs:14071`).

## What this round added

- `crates/ec-av1/src/stream.rs:8180,8316` -- the gate's `force_integer_mv frames > 0` and
  `forced-2-symbol blocks > 0` assertions were AGGREGATE over the 6 cq/step cells, so one
  live cell could have hidden five vacuous ones (class `gate-blind-to-feature`). Both
  counters are now snapshotted per attempt and asserted right after that attempt's pixel
  compare. Both arms stay green => all 6/6 cells of each arm really carry the header state
  and the forced alphabet.
- `crates/ec-av1/src/decode.rs:12398` -- charter asked for `ref_is_scaled` to become
  `x || y` like `av1_is_scaled`. NOT DONE as code, deliberately: a reference whose height
  differs from this frame's true size is refused before any tile decodes
  ("a reference picture whose height does not match this frame's own true size",
  `decode.rs:14681`), so the y term is unreachable dead code today. Recorded as a comment
  at the site naming the refusal and handing the y term to whoever lifts it
  (ladder rung 1: does not need to exist).

## Gate

`a_real_force_integer_mv_warp_stream_decodes_pixel_exact` (8-bit) and
`a_real_force_integer_mv_warp_10bit_stream_decodes_pixel_exact` (10-bit),
`crates/ec-av1/src/stream.rs:8160`. Real `aomenc` (`~/.cache/aom-oracle/build/aomenc`),
`--tune-content=screen --enable-warped-motion=1 --enable-obmc=1 --enable-global-motion=0`,
64x64x20 translating screen-content tiles, 6 cells (`cq` 32/45/55 x `step` 4/8), every
decoded frame pixel-compared against ffmpeg.

```
EC_AV1_REQUIRE_AOMENC=1 CARGO_TARGET_DIR=$HOME/.cache/cargo-target-fimv \
  cargo test -p ec-av1 --lib -- force_integer_mv --nocapture --test-threads=1
```

EVIDENCE: $HOME/.cache/fimv-suite.log | aomenc --tune-content=screen --enable-warped-motion=1 --enable-obmc=1 --enable-global-motion=0, 6 cq/step cells x 20 frames, each frame Y/U/V compared to ffmpeg | 8-bit 6/6 pixel-exact, 0 refusals, force_integer_mv frames=114, forced-2-symbol blocks=342; 10-bit identical 6/6 / 114 / 342
EVIDENCE: $HOME/.cache/fimv-suite.log | sibling re-run `cargo test -p ec-av1 --lib -- warp obmc superres scaled` | 20 passed, 0 failed, 2 ignored (pre-existing: 10-bit compound MC defect; pinned-fixture test)

## Refusals

None lifted this lane -- the defect was a wrong CDF alphabet, not a refused capability.
`refusal_inventory` (3 tests) and `gate_coverage` (9 tests) stay green with no edit needed.

## Suite totals

`EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib -- --test-threads=3`, log
`$HOME/.cache/fimv-suite.log`: **300 passed, 1 failed, 27 ignored, 0 filtered out** (933s). The single failure is the pre-existing-on-main one below.

`decode::tests::nz_map_ctx_offset_tables_match_the_rect_rule` fails, and is PRE-EXISTING ON
MAIN: reproduced at `df5d630` in a detached verify worktree with its own target dir --
`32x64 nz_map offset at display (row 0, col 2) left: 6 right: 11` (`decode.rs:16315`). This
lane's diff never touches `nz_map` (`git diff df5d630..HEAD -- decode.rs | grep -c nz_map`
= 0).

## Residue

- deferred(whoever lifts the reference-height refusal at `decode.rs:14681`): the
  `av1_is_scaled` y_scale_fp term in `warp_eligible`. Unreachable today, documented at the
  site.
- accepted (not this lane): `nz_map_ctx_offset_tables_match_the_rect_rule` red on main.
- accepted: `a_real_compound_global_warp_10bit_stream_decodes_pixel_exact` stays `#[ignore]`
  (pre-existing 10-bit compound MC defect).
