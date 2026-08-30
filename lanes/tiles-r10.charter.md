# lane-tiles r10 — finish r9's tile-group gate

At 5524499. Read `lanes/tiles-r9.charter.md` (still your charter — r9 never
reported) and `lanes/tiles-r8.report.md`.

r9 merged main cleanly (8d08ef0, its own commit) and then died mid-edit while
replacing a tile-group test function. I committed its in-flight `stream.rs` edit
verbatim; **it may not even compile.** Check that first, and fix or revert that
one edit before anything else.

Then the r9 charter's job, unchanged: several tile-group OBUs per frame, and
non-uniform tile spacing (`uniform_tile_spacing_flag == 0`), each STALENESS-
CHECKED before any new code. r8's finding is the method — it found
`context_update_tile_id != 0` already worked, guarded only by a stale comment
claiming a refusal that existed nowhere in the crate, and closed it with a live
proof rather than new machinery. Grep for the refusal, prove the capability is
genuinely absent, and only then build. If a capability is missing: gate below
`decode_stream` first, then lift, updating `refusal_inventory.rs` in the same
commit. If aomenc cannot be made to emit a case, say so with the commands you
tried rather than substituting a hand-built fixture as equivalent
([[fixture-proves-symbol-not-signal]]).

## Budget discipline (I am serious about this)
You have 75 turns and they do NOT reset if you are resumed. At roughly turn 55,
STOP starting new work: commit what is green, and write your report. Five rounds
in the last batch died mid-edit with nothing reported, and each cost a whole
round to recover. A round that ends with an honest report beats one that ends
five turns deeper into an edit.

Hard rules: `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-tiles`; foreground
`nice -n 19 cargo ... -j4`; every `cargo test` a timeout >= 600000 ms; fixtures
through `gradients_source(seed, w, h, tail)`; ffmpeg generates bounded with `-t`;
`EC_AV1_REQUIRE_AOMENC=1`; aomenc `--threads=1 --row-mt=0 --sb-size=64`. Sibling
worktrees have live agents — never build in or edit them. Never push, never merge
into main. End with `lanes/tiles-r10.report.md`, VERDICT on line 1.
