# lane-palette r6 — run the bisect

At e5e8e6d. Read `lanes/palette-r5.report.md`.

## Where you stand
r5 merged main into this branch and resolved all six `decode.rs` conflicts plus
`gate_coverage.rs` — the tree now carries main's CDEF, chroma and multi-tile
work, `NEVER_EXERCISED` is down to `enable-intrabc` alone, and two refusal
strings the merge surfaced are pinned. Suite 244/0. **No merge debt remains**,
so this round is entirely the bisect that r4 built the instrument for.

## The one job
Palette-Y decodes its colours and index map, `palette_hits` fires, and the block
then refuses by name because the reconstructed pixels do not match libaom.
r3 checked `read_palette_colors_y`, `read_uniform`,
`palette_color_index_context` and the `PALETTE_Y_COLOR_INDEX` table
line-for-line against the oracle source — all match. So do not re-read tables
(class `worker-cap-spent-reading`). Use the instrument:

1. r4's `EC_TRACE_PALETTE=1` rung (rungs 6/6b in
   `scripts/instrument-aom-oracle.sh`, aomdec already rebuilt on this box)
   prints, inside libaom's `decode_color_map_tokens`, per-pixel
   row/col/ctx/n/rng before and color_idx/rng after, plus the `map[0]` uniform
   read.
2. Add the matching trace inside our `decode_color_index_map`
   (decode.rs ~2182 before r5's merge; find it by name).
3. Regenerate the gate's exact fixture stream ONCE and run both traces over it.
4. Diff RANGES, never `tell()` (class `compare-range-not-tell`). Reference range
   unchanged where ours moves = we read a symbol it never wrote; theirs moves
   and ours does not = we skipped one (class `equal-range-means-unread`).
5. **If the ranges match to the end of the block, the reads are right** and the
   bug is in how the palette is applied — the `PALETTE_PRED` thread-local's
   lifetime, the buffer write, per-TU slicing, or plane mixing. That is the more
   likely outcome given r3's table audit, so check it early rather than last.

Then remove the refusal IN THE SAME COMMIT as the gate that proves pixel
exactness — not before. Two sibling lanes lifted refusals on a green suite this
batch and one had a firing counter that was never incremented, so its own assert
was blind. A green suite cannot prove a path nothing takes.

## Then
Palette UV, the rect-strip `palette_bsize_ctx` refusal (decode.rs ~1940), then
intrabc — which is the last entry on `gate_coverage.rs`'s `NEVER_EXERCISED`.
One commit each, each with its own hard-asserted firing count.

Merge note: main is at 93d9510. Report every refusal string you add or remove,
verbatim.

Hard rules: `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-palette`; foreground
`nice -n 19 cargo ... -j4`; every `cargo test` a timeout >= 600000 ms. Sibling
worktrees have live agents — never build in or edit them, and do NOT create
additional worktrees. Never push, never merge into `main`. 75-turn cap, does not
reset: COMMIT AT EVERY GREEN STEP. End with `lanes/palette-r6.report.md`,
VERDICT on line 1.
