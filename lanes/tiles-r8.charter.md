# lane-tiles r8 — tile rows

At 5255492. r7 is MERGED into main (93d9510); main is at 92d8beb or later.
Read `lanes/tiles-r7.report.md` first. Start with `git merge main` — main has
since landed a `bit_depth != 8` refusal, three reworded partition refusals and
a dead-binding cleanup in `decode_stream`; resolve, suite green, commit, and
only then start new work.

## The round: tile ROWS
`tile_info.rows > 1` is still refused in `decode_stream`, correctly and with no
false claim attached. Close it the exact way r7 closed columns, in this order —
each step committed before the next begins:

1. **Bypass gate first.** A gate that drives the tile decode directly, below
   `decode_stream`, so the refusal cannot short-circuit the code the gate is
   supposed to be measuring. That is class [[refusal-short-circuits-its-own-code]]
   and it cost this project a round already: lane-lr's report claimed symbols
   were read while its own counter said 0/40, because the refusal ran first.
2. **`decode_stream`-level gate**, pixel-exact vs ffmpeg, loop filter ON.
   Deblocking crosses tile ROW boundaries by spec default just as it crosses
   column ones — do not add a `--loopfilter-control=0` bypass, and if the
   horizontal boundary fails, that is the round's real finding.
3. **Then, and only then, lift the refusal**, and update
   `refusal_inventory.rs`'s pinned list in the same commit.

Recipe: `--tile-rows=1` (→ TileRows=2) mirroring r7's `--tile-columns`, and a
fixture at least 128 px tall so two rows of 64px superblocks actually exist.
Verify from a header parse that aomenc really emitted TileRows=2 before
trusting any pixel match — r7 did this and it is why its numbers hold. Hard-
assert `tile_hits`: r7 found `decode_inter_frame_tile_with_cdfs` never
incremented `TILE_HITS`, so an assert there was vacuously blind. Check the
row path has the same increment before you rely on it.

A branch that lifts a refusal without a gate does not merge to main.

## If turns remain
`context_update_tile_id != 0`, several tile-group OBUs per frame, non-uniform
tile spacing — in that order. Each is a refusal today; each needs a gate before
a lift.

Hard rules: `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-tiles`; foreground
`nice -n 19 cargo ... -j4`; every `cargo test` a timeout >= 600000 ms; fixtures
through `gradients_source(seed, w, h, tail)` and every ffmpeg generate bounded
with `-t`; `EC_AV1_REQUIRE_AOMENC=1`; aomenc `--threads=1 --row-mt=0
--sb-size=64` (this decoder hardcodes 64px superblocks). Sibling worktrees have
live agents — never build in or edit them. Never push, never merge into main.
75-turn cap, does not reset: COMMIT AT EVERY GREEN STEP. End with
`lanes/tiles-r8.report.md`, VERDICT on line 1.
