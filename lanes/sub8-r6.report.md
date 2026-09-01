# lane-sub8 report (round 6) -- verifier FAIL closed

## The defect the verifier found (root cause)

The mi-granular mode maps added in r2/r5 (`sub8_mode_col`/`sub8_mode_row`,
`uv_mode_col`/`uv_mode_row`) are FRAME-wide arrays read with a FRAME-relative
availability guard (`mi_r > 0` / `mi_c > 0`) and never reset per tile. Every
other neighbour site in `decode.rs` guards tile-relatively
(`mi_r > self.tile_row0_mi`, e.g. :2240, :3222, :6152). With more than one
tile, the first block of tile N read a `(row, mode)` entry left by the tile to
its LEFT (or above) as if it were its own neighbour -- wrong intra-mode
context / wrong chroma edge-filter type, and from there wrong pixels.

## What changed (all in the lane worktree, branch `lane-sub8`)
- `crates/ec-av1/src/decode.rs:2533,2540,2574,2578` -- the four reads now guard
  with `self.tile_row0_mi` / `self.tile_col0_mi`.
- `crates/ec-av1/src/decode.rs:2000` (`start_tile`) -- clears
  `sub8_mode_col`/`uv_mode_col` over the tile's own mi column span
  (`col0_mi..col1_mi`), alongside the existing `above_*` reset.
- `crates/ec-av1/src/decode.rs:2286` (`start_row`) -- clears
  `sub8_mode_row`/`uv_mode_row` with the rest of the left context.
- `crates/ec-av1/src/decode.rs:3763` -- `decode_leaf_rect` (the 16x8/8x16
  strips) now calls `record_mode_mi`/`record_uv_mode_mi` over its real span;
  before, the caller patched only the coarse 16x16 `above_mode`/`left_mode`
  slot, so a neighbour of the TOP strip read the BOTTOM strip's mode.
  (Sweep item 1 of the verifier's list.)
- `crates/ec-av1/src/decode.rs:5326` -- deleted the stale `corner-cut` comment
  claiming the uv neighbours are coarse; they are mi-exact since r5.
- `crates/ec-av1/src/stream.rs:7259..` -- gate rewritten: three arms
  (single tile / `--tile-columns=1` at 128x64 / `--tile-rows=1` at 64x128),
  each needing 4 firing + pixel-exact runs; `tile_hits()` delta >= 2 asserted
  on the tile arms (gate-blind-to-feature); the `Err` arm now ASSERTS the
  message starts with `unsupported: ` -- any other decode error panics
  instead of counting as a tolerated refusal; the fixture's `noise` filter
  carries `all_seed={seed}`.
- `lanes/sub8-r5.report.md:45` -- the deleted refusal string named correctly
  ("a partition below 8x8 ...").

## Gate

`EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib sub8` -> **ok, 1 passed**
(3 arms x 4 firing+pixel-exact runs).

Regression value proved by reverting only the guard+reset hunks: the same gate
FAILS (`U vs ffmpeg (seed=139 cq=12)`, stream.rs:7394) -- so this gate catches
the defect, it is not a pass-by-construction arm.

EVIDENCE: <scratchpad>/bad207.obu (sha256 0f78db8b...), bad228.obu (sha256 6a73db99...) | `decode_probe <obu> out.yuv` + `ffmpeg -i <obu> -f rawvideo -pix_fmt yuv420p` on this branch | both 12288/12288 bytes equal (verifier's pre-fix .mine.yuv: 221 and 158 differing bytes, all at x>=64)
EVIDENCE: gate run above | 3-arm gate incl. --tile-columns=1 / --tile-rows=1, tile_hits delta >= 2 asserted | ok; with the guard+reset hunks reverted the same gate FAILS at seed=139 cq=12 (U plane)
EVIDENCE: <scratchpad>/hg_r6.obu | `ffmpeg -t 0.4 -c:v copy -f obu` from the Hunger Games film, `decode_probe` on this branch | REFUSED at "a HORZ/VERT partition below 8x8 (... 4x8/8x4 need a real rectangular transform)" -- unchanged from r5, and past main's "a partition below 8x8"
EVIDENCE: fixture reproducibility | the gate's lavfi source (seed 207, 128x64) rendered twice with and without `all_seed=207` | with: d3c4b3e3... both runs; without: 5619b3ee... both runs -- ffmpeg's `noise` was in fact deterministic in this pair of runs, `all_seed` pins it explicitly rather than relying on that

## Refusals
None lifted, none added this round; `refusal_inventory` and `gate_coverage`
stay green (totals below).

## Suite
`EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib`: SUITE_TOTALS

## Open / disposition
- deferred(an inter lane) -- `decode.rs:8547-8548` (`up_available`/
  `left_available` in the inter path) and `mvstack.rs:727,733,742,756,1486,
  1492,1501,1515` still use frame-relative `mi_row > 0` / `mi_col > 0` where
  libaom uses `tile->mi_row_start`/`mi_col_start`. Same shape as this round's
  defect but in code this lane never touched and no intra gate reaches;
  unblocked by a multi-tile INTER gate.
- deferred(an inter-frame gate whose chroma neighbour codes a smooth
  `uv_mode`) -- carried from r5: `decode.rs:12084,12096` still pass a
  hardcoded `false` chroma edge-filter type.
- accepted: `smooth_uv_neighbour` falls back to the coarse `SUB` slot when no
  block recorded the exact neighbouring mi.
