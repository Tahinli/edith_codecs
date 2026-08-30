# lane-tiles r9 — the last two tile refusals, staleness-checked first

At 5255492, MERGED into main (9a632d7). Tile rows are closed, and so is
`context_update_tile_id != 0` — which r8 found already worked, guarded only by a
stale comment claiming a refusal that existed nowhere in the crate. Read
`lanes/tiles-r8.report.md`.

Start with `git merge main` (a8724cb — delta_q/delta_lf, key-frame superres,
palette-Y, a `bit_depth != 8` refusal), resolve, suite green, commit.

## The round: two capabilities, each staleness-checked BEFORE any new code
r8's own finding sets the method: **grep for the refusal first and prove the
capability is genuinely absent before building machinery for it.** A user-visible
"not supported" is a claim, and a claim with no test behind it is a bug either
way — either the code is missing or the message is a lie.

1. **Several tile-group OBUs per frame.** Check whether `decode_stream` and the
   OBU walk actually assume one tile group. If a refusal exists, gate then lift;
   if it does not, prove the capability live with a real aomenc stream that emits
   more than one tile group and hard-assert it, exactly as r8 did for
   `context_update_tile_id`.
2. **Non-uniform tile spacing** (`uniform_tile_spacing_flag == 0`). Same
   treatment. Note aomenc may not emit it at all from the CLI — if you cannot
   make a real encoder produce one, say so with the commands you tried, and do
   not paper over it with a hand-built fixture presented as equivalent
   ([[fixture-proves-symbol-not-signal]]).

A branch that lifts a refusal without a gate does not merge to main.

## If both turn out to be closed already
Then the next tile-shaped gap is worth naming rather than inventing work: sweep
`decode.rs` and `stream.rs` for every remaining assumption that the frame is one
tile — tile-boundary MV stack bounds, CDF context selection, loop-restoration
stripe boundaries — and report which are proven by a gate today versus which
merely have not been contradicted. That inventory is the deliverable.

Hard rules: `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-tiles`; foreground
`nice -n 19 cargo ... -j4`; every `cargo test` a timeout >= 600000 ms; fixtures
through `gradients_source(seed, w, h, tail)`; ffmpeg generates bounded with `-t`;
`EC_AV1_REQUIRE_AOMENC=1`; aomenc `--threads=1 --row-mt=0 --sb-size=64`. Sibling
worktrees have live agents — never build in or edit them. Never push, never merge
into main. 75-turn cap, does not reset: COMMIT AT EVERY GREEN STEP. End with
`lanes/tiles-r9.report.md`, VERDICT on line 1.
