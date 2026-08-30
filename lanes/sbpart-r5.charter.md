# lane-sbpart r5 — the rect64 bisect, third attempt

At 2f51dc1. Read `lanes/sbpart-r4.charter.md` and `lanes/sbpart-r2.report.md`.

Two rounds have now died at the cap on this without reporting. r4 stopped while
adding "one more targeted trace print to pin down whether the desync is in symbol
reading or purely in reconstruction" — I committed its edits verbatim, trace
prints included, state unverified. **Answer that exact question first**, because
it splits the search in half:
- If the ENTROPY stream desyncs (a symbol read that libaom does not write, or
  vice versa), it is a range-ladder problem: compare msac RANGE element by
  element against the oracle, never `tell()` ([[compare-range-not-tell]]), and
  remember that a reference range that does not move where ours does means we
  read a symbol it never wrote ([[equal-range-means-unread]]).
- If the symbols match and only pixels differ, it is reconstruction: the
  corner-embed stride versus `dequant_and_inverse_typed_wh`'s w/h ordering for
  64x32 vs 32x64, or `default_scan(TX32)` versus the shared `scan32`. If you
  touch scanning, sweep the transposed copy in the SAME round
  ([[scan-weights-cross-axis]]).

Report which of the two it is even if you fix nothing else. That answer is worth
more than another partial trace, and it is what the last two rounds failed to
deliver.

Then merge `main` into this branch (now 913df61 — loop restoration filters
landed on top of tile rows, superres, palette-Y and delta_q/delta_lf) and resolve
it yourself. Watch for the merge shape that has now bitten three times: git
merges the text cleanly while a newer gate calls a decoder whose signature this
branch changed. Compile after every merge, not just at the end.

Gate-recipe facts already paid for: `--enable-tx-size-search=0` is REQUIRED, and
`--enable-ab-partitions=0` does NOT gate AB at 64x64, so use plain
`gradients_source`.

## Budget discipline (I am serious about this)
You have 75 turns and they do NOT reset if you are resumed. At roughly turn 55,
STOP starting new work: commit what is green, and write your report. Five rounds
in the last batch died mid-edit with nothing reported, and each cost a whole
round to recover. A round that ends with an honest report beats one that ends
five turns deeper into an edit.

Hard rules: `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-sbpart`; foreground
`nice -n 19 cargo ... -j4`; every `cargo test` a timeout >= 600000 ms;
`EC_AV1_REQUIRE_AOMENC=1`; aomenc `--threads=1 --row-mt=0 --sb-size=64`. Oracle
rungs 6, 7, 8, 8b are taken — take 9; the oracle is SHARED, env-gated rungs only.
Sibling worktrees have live agents — never build in or edit them. Never push,
never merge into main. End with `lanes/sbpart-r5.report.md`, VERDICT on line 1.
