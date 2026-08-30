# lane-palette r5 — merge main, then run the bisect

At 323456a. Read `lanes/palette-r4.report.md`.

## Job 1 — merge `main` into this branch and resolve, before anything else
`git merge main` from this worktree. It conflicts in six places in `decode.rs`
(main has since landed multi-tile key-frame decode, the chroma smooth/paeth work
and CDEF index reads, all of which touch `Neighbours` / `PlaneBuf` / the block
readers) and one in `gate_coverage.rs`. **You** resolve it, not the orchestrator:
you know which side owns each hunk here, and a wrong resolution in this file is
a silent decode bug. Then the full suite green, then COMMIT. Main is at 53f5358.

While resolving, two things main added that you must keep:
- `gate_coverage.rs`'s `NEVER_EXERCISED` — your removal of `enable-palette` is
  correct and stays (a real gate enables it now); main still lists
  `enable-intrabc`.
- `refusal_inventory.rs` — pins every decode-path refusal string. Your reworded
  palette-Y string must be listed there or the suite fails, which is the point.
  Main also reworded three refusals that falsely claimed "this encoder never
  writes" a case aomenc demonstrably writes; do not reintroduce that phrasing.

## Job 2 — the bisect r4 built the instrument for
r4's `EC_TRACE_PALETTE=1` rung (rungs 6/6b in
`scripts/instrument-aom-oracle.sh`, aomdec rebuilt) prints per-pixel
row/col/ctx/n/rng before and color_idx/rng after inside libaom's
`decode_color_map_tokens`, plus the `map[0]` uniform read. Add the matching
trace inside our `decode_color_index_map` (decode.rs ~2182), regenerate the
gate's exact fixture stream ONCE, and diff.
- CLASS `compare-range-not-tell`: compare RANGE, never `tell()`.
- CLASS `equal-range-means-unread`: reference range unchanged where ours moves =
  we read a symbol it never wrote; theirs moves and ours does not = we skipped
  one. Tables are already cleared line-for-line, so a table is the LAST suspect.
- If the ranges match to the end of the block, the reads are right and the bug
  is in how the palette is applied — `PALETTE_PRED` / the buffer write
  (decode.rs ~3347), per-TU slicing, or plane mixing.
Then remove the refusal, make the gate assert pixel-exactness, and COMMIT.

## Then
Palette UV, the rect-strip `palette_bsize_ctx` refusal (decode.rs ~1940), then
intrabc — one commit each, each with its own hard-asserted firing count.

Hard rules: `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-palette`; foreground
`nice -n 19 cargo ... -j4`; every `cargo test` a timeout >= 600000 ms. Sibling
worktrees have live agents — never build in or edit them, and do NOT create
additional worktrees. Never push, never merge into `main`, never touch `main`
itself (merging main INTO this branch is what job 1 asks for and is fine).
75-turn cap, does not reset: COMMIT AT EVERY GREEN STEP. End with
`lanes/palette-r5.report.md`, VERDICT on line 1.
