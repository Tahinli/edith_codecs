# lane-palette2 r4 — palette on a rectangular strip, from a clean base

At main (8440024). **I reset this branch to main.** r3's in-flight edits are
preserved at tag `palette2-r3-wip` if you want to look, but they conflicted with
the u16 sample widen that has since landed, and they were mid-mechanical-edit
(threading `allow_intrabc` through four call sites) rather than hard-won
knowledge. Start fresh; re-doing that plumbing on top of the widen is cheaper
than untangling it.

Read `lanes/palette2-r1.charter.md` for the lane's scope. Step 1 (UV palette) is
CLOSED and merged. This round is step 2:

    "a HORZ/VERT intra strip in a screen-content frame
     (palette syntax is consumed for square blocks only)"

`smptebars` and `rgbtestsrc` at plain default aomenc settings both stop exactly
there — that is measured, not assumed, so a real gate is easy to build.

`palette_bsize_ctx` and the palette size/context derivation assume a square
block; libaom derives both from width AND height. Check what it actually does
for a rect block before adapting the square path. Two classes apply: a table
narrowed to a pinned row breaks when its indexing field moves
([[cdf-row-held-constant]]), and a neighbour context read from ONE cell is right
only under uniform block size — a strip needs a gathered span
([[context-read-from-one-cell]]).

Note what a sibling lane just found in the same neighbourhood: libaom indexes
`av1_nz_map_ctx_offset` by the RAW, un-adjusted `tx_size`, and the 32x64/64x32
tables genuinely differ from the square one. If your rect-strip work touches a
size-indexed table, check whether libaom indexes it by the raw or the adjusted
size before assuming.

Order: gate FIRST below `decode_stream`, hard-asserting the rect-strip palette
path fired; then the implementation; then lift the refusal and update
`refusal_inventory.rs` in the SAME commit. Declare every new refusal string you
add — `refusal_inventory`'s own test fails the build otherwise, and r3 left two
undeclared for me to clean up.

Step 3 ("a palette block with a split luma transform", reached by `life`) only
if step 2 is committed green with budget to spare.

## Budget discipline
75 turns, no reset on resume. At about turn 55, stop starting new work: commit
what is green and write your report.

Hard rules: `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-palette2`; foreground
`nice -n 19 cargo ... -j4`; every `cargo test` a timeout >= 600000 ms; ffmpeg
generates bounded with `-t`; `EC_AV1_REQUIRE_AOMENC=1`; aomenc
`--threads=1 --row-mt=0 --sb-size=64`. Oracle rung 12 is yours; the oracle is
SHARED, env-gated rungs only. Sibling worktrees have live agents — never build in
or edit them. Never push, never merge into main; I handle merges. End with
`lanes/palette2-r4.report.md`, VERDICT on line 1.
