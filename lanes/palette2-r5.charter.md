# lane-palette2 r5 — write the gate

At 05f6db8. r4 implemented the rect-strip palette path and hit its cap with
"compiles clean, now let's write the gate test". I committed its tree verbatim;
**it compiles but is otherwise unverified by me, and it has NO gate**, which
means right now it is exactly the thing this project refuses to ship: a
capability claim with nothing behind it.

## The job, in one sentence
Write the gate for what r4 built, and let it tell you whether r4 is right.

Specifics:
- `smptebars` and `rgbtestsrc` at plain default aomenc settings both reach the
  rect-strip palette case — that is measured, so no fixture hunting is needed.
  Add `--enable-palette=1` explicitly: `gate_coverage.rs` only counts a tool as
  exercised on a positive flag.
- Drive the tile decode BELOW `decode_stream` so a surviving refusal cannot
  short-circuit the code you are measuring
  ([[refusal-short-circuits-its-own-code]]).
- Hard-assert that the rect-strip palette path actually fired before comparing
  pixels ([[gate-blind-to-feature]]) — a hit counter, not just a pixel match.
- Pixel-exact vs ffmpeg. If it mismatches, that is r4's implementation being
  wrong, which is a perfectly good outcome for this round: bisect it with a
  range ladder against the oracle (rung 12 is yours), and say so plainly.
- Only with the gate green: lift the refusal and update `refusal_inventory.rs`
  in the SAME commit. Check what r4 already changed in that file — it edited it
  without a gate, so treat its edits as unverified too.

Do not start step 3 (palette with a split luma transform) this round.

## Budget discipline
75 turns, no reset on resume. At about turn 55, stop starting new work: commit
what is green and write your report.

Hard rules: `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-palette2`; foreground
`nice -n 19 cargo ... -j4`; every `cargo test` a timeout >= 600000 ms; ffmpeg
generates bounded with `-t`; `EC_AV1_REQUIRE_AOMENC=1`; aomenc
`--threads=1 --row-mt=0 --sb-size=64`. The oracle is SHARED — env-gated rungs
only. Sibling worktrees have live agents — never build in or edit them. Never
push, never merge into main; I handle merges. End with
`lanes/palette2-r5.report.md`, VERDICT on line 1.
