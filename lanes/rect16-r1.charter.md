# lane-rect16 r1 — rectangular partitions below 16x16

Fresh lane off main (170a5a3).

## Why this lane exists — measured
Plain default-settings aomenc over lavfi `mandelbrot`
(`--cpu-used=4 --end-usage=q --cq-level=32`, no `--enable-*` flags, 192x128)
refuses on frame 0 with **"a partition below 16x16 other than a clean split"**.
That refusal used to end "(this encoder never writes one)"; I reworded it on
main at e067e32 when this probe disproved it. Reproduce it yourself first —
`cargo run -p ec-av1 --example decode_probe -- <stream.obu>`, extracting with
`ffmpeg -i <ivf> -c:v copy -f obu` — so you are looking at a live failure, not
my sentence about one.

## Scope
The HORZ/VERT arms at 8x8 and below: `PARTITION_HORZ`/`PARTITION_VERT` under a
16x16 parent, and whatever `decode_block` needs to code an 8x4/4x8 leaf. A
sibling lane (lane-sbpart) owns the same shape at 64x64 and is mid-bisect on a
luma mismatch there — **do not touch its worktree**, but do read
`git log`/`git show` for `decode_block_rect` and `decode_block_rect64` first:
the rectangular transform, scan and context machinery already exists, and this
may be a wiring job rather than a new primitive. If so, say that plainly instead
of building a second copy ([[square-only-transform-ceiling]]).

Two classes to keep in view, both paid for by this repo:
- A rectangular scan's weight or step can use the CROSS axis; square candidates
  hide every axis swap, so sweep the transposed copy in the SAME round
  ([[scan-weights-cross-axis]]).
- A neighbour context read from ONE cell is right only under uniform block size;
  strips need a gathered span ([[context-read-from-one-cell]]).

## Order
1. Reproduce the refusal, and identify the exact failing BLOCK — size, position,
   partition symbol — not just the frame's configuration. A refusal written from
   a mismatching frame's header names a correlate, not the defect
   ([[refusal-names-a-correlate]]).
2. Gate first, below `decode_stream`, hard-asserting the new partition arm fires.
3. Implement, bisecting any pixel mismatch with a range ladder against the
   oracle (compare msac RANGE, never `tell()`).
4. Lift the refusal and update `refusal_inventory.rs` in the same commit, only
   once the gate is pixel-exact.

## Budget discipline
75 turns, and they do NOT reset if you are resumed. At about turn 55, stop
starting new work: commit what is green and write your report.

Hard rules: `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-rect16`; foreground
`nice -n 19 cargo ... -j4`; every `cargo test` a timeout >= 600000 ms; fixtures
through `gradients_source(seed, w, h, tail)` where synthetic will do; every
ffmpeg generate bounded with `-t`; `EC_AV1_REQUIRE_AOMENC=1`; aomenc
`--threads=1 --row-mt=0 --sb-size=64`. Oracle rungs 6, 7, 8, 8b are taken — take
10 (9 is spoken for); the oracle is SHARED, env-gated rungs only. Sibling
worktrees have live agents — never build in or edit them. Never push, never merge
into main. End with `lanes/rect16-r1.report.md`, VERDICT on line 1.
