# lane-palette2 r1 — the palette frontier

Fresh lane off main (170a5a3). Palette-Y already decodes pixel-exact; the lane
that landed it is merged and retired. Read its work in `git log` for
`decode_color_index_map` and `read_intra_mode`'s palette branch before editing.

## Why this lane exists — measured, not guessed
I probed five content classes through plain default-settings aomenc
(`--cpu-used=4 --end-usage=q --cq-level=32`, no `--enable-*` flags) with
`cargo run -p ec-av1 --example decode_probe`. Three of the five stop on a
palette refusal:
- `testsrc2`   -> "a block that actually uses a palette (UV) -- reconstruction
  is out of scope"
- `smptebars` and `rgbtestsrc` -> "a HORZ/VERT intra strip in a screen-content
  frame (palette syntax is consumed for square blocks only)"
- `life`       -> "a palette block with a split luma transform (round 1)"

Palette is the single biggest thing between this decoder and an ordinary AV1
stream. Close those three, in that order — each is a gate-then-lift.

## Order
1. **UV palette reconstruction.** The reads already exist and are correct (the
   previous lane ported `palette_uv_mode`/`palette_uv_size`; it deliberately
   skipped `read_palette_colors_uv` because the refusal aborts immediately, so
   that reader still needs writing). Then reconstruct both chroma planes from
   the palette and its index map.
2. **Palette on a rectangular (HORZ/VERT) intra strip.** `palette_bsize_ctx`
   and the size/context derivation assume a square block today.
3. **Palette block with a split luma transform.**

Each step: gate FIRST against a real aomenc stream, driving the tile decode
below `decode_stream` so the refusal cannot short-circuit the code you are
measuring ([[refusal-short-circuits-its-own-code]]); then the implementation;
then lift the refusal and update `refusal_inventory.rs` **in the same commit**.
A branch that lifts a refusal without a gate does not merge to main. The gate
must hard-assert that the feature actually fired — a pixel match on a stream
that never used a palette proves nothing ([[gate-blind-to-feature]]).

Content that reaches each case is known: `testsrc2` for UV, `smptebars` or
`rgbtestsrc` for the rect strips, `life` for the split transform, all at the
default settings above and `--sb-size=64` (this decoder hardcodes 64px
superblocks). `gate_coverage.rs` requires an explicit `--enable-palette=1` for a
gate to count as exercising palette.

## Budget discipline
75 turns, and they do NOT reset if you are resumed. At about turn 55, stop
starting new work: commit what is green and write your report. Five rounds in a
recent batch died mid-edit with nothing reported, each costing a whole round to
recover.

Hard rules: `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-palette2`; foreground
`nice -n 19 cargo ... -j4`; every `cargo test` a timeout >= 600000 ms; fixtures
through `gradients_source(seed, w, h, tail)` where a synthetic source will do;
every ffmpeg generate bounded with `-t`; `EC_AV1_REQUIRE_AOMENC=1`; aomenc
`--threads=1 --row-mt=0 --sb-size=64`. Oracle rungs 6, 7, 8, 8b are taken — take
9; `~/.cache/aom-oracle` is SHARED with sibling lanes, so env-gated rungs in
`scripts/instrument-aom-oracle.sh` only, never a throwaway patch left in the
tree. Sibling worktrees have live agents — never build in or edit them. Never
push, never merge into main. End with `lanes/palette2-r1.report.md`, VERDICT on
line 1.
