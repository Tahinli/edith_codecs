# lane-sbpart r10 — the truncated-corner reconstruction defect

At d9a2f81. Read `lanes/sbpart-r9.report.md` — it carries the ranking table, the
repro, and the shape of the work it had to throw away.

## First, a correction to how I have been chartering this lane
r9 implemented the part32 AB arms (HORZ_A/HORZ_B/VERT_A/VERT_B), got them
compiling and geometrically correct, then **reverted them with `git stash drop`**
because the gate went red — and the gate went red for a reason that had nothing
to do with its code. That was my fault, not r9's: my charters kept saying "a
branch that lifts a refusal without a gate does not merge to main", and it read
as "do not land red". To be explicit:

**Landing red ON THIS BRANCH is fine and expected. Only MERGING red is
forbidden, and I do the merges.** Commit compiling work even when a gate fails,
with the failure named in the commit message. Never `git stash drop` or
`git checkout` away a day's work to keep a branch tidy — three rounds across
this project have now destroyed real work that way.

r9's AB arms mirrored the already-merged inter lane-partab arms and composed
`decode_block` (proven 16x16 square leg) with `decode_block_rect` (proven
32x16/16x32 strip leg). If you can reconstruct that from the report cheaply, do;
if not, it is not this round's job.

## The job
r9's real finding: `decode_block_rect64` (`decode.rs:3409-3505`) has a
**pre-existing real-residual reconstruction bug for TX_32X64/TX_64X32
truncated corners**. It was invisible because a later refusal in the same frame
always short-circuited the pixel comparison; lifting the part32-AB refusal let
the decode run far enough to reach it.

That makes it row 1 of the ranking table's blocker: it gates both the part32-AB
item and any future SB-level AB / HORZ_4 / VERT_4 work, since all of them pass
through the same corner-truncation path. It is the biggest single item in the
lane (13 of 25 refusal hits sit behind it).

Repro from r9: seed 42, superblock 1, VERT split, `mi_col` 16 and 24 —
strip 1 (flat, all-zero residual) diverges mid-block; strip 2 (real nonzero
residual) produces a wrong-shaped curve versus both ffmpeg and the oracle. Use
`EC_AV1_GATE_DUMP` plus the oracle's `EC_TRACE_MODE`/`EC_TRACE_COEFF` rungs
(rung 11 is yours and live) to range-ladder it.

Note the shape of the evidence: the symbols were already proven right for this
path in r8, and a flat strip diverging means the defect is in **reconstruction
geometry** — the corner embed, the stride, or which sub-rectangle of the
transform output is written — rather than in the coefficients. Check what
libaom actually writes for a truncated 32x64 corner before adjusting ours.

## Budget discipline
75 turns, no reset on resume. At about turn 55, stop starting new work: commit
what you have — red gate included, named as such — and write your report.

Hard rules: `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-sbpart`; foreground
`nice -n 19 cargo ... -j4`; every `cargo test` a timeout >= 600000 ms;
`EC_AV1_REQUIRE_AOMENC=1`; aomenc `--threads=1 --row-mt=0 --sb-size=64` and this
gate needs `--enable-tx-size-search=0`. The oracle is SHARED — env-gated rungs
only. Sibling worktrees have live agents — never build in or edit them. Never
push, never merge into main. End with `lanes/sbpart-r10.report.md`, VERDICT on
line 1.
