# lane-sbpart r9 — stage 2

At main-merged HEAD. r8 CLOSED the rect64 luma corner defect (nz_map offsets are
indexed by the raw `tx_size`; the 32x64 and 64x32 tables genuinely differ from
the square one) and it is merged to main. Read `lanes/sbpart-r8.report.md`.

The gate now reads 15/40 pixel-exact, 25 named refusals, 0 mismatches,
`sb_rect_hits=36`. **Twenty-five refusals is the number to attack**: each one is
a case a real default-ish aomenc stream produces that this decoder still will
not decode. Start by listing them with counts — which refusal fires how often
across the 40 attempts — and put that table in your report. That ranking decides
this round's order, and it is worth more to the lane than any single fix.

Then take them in frequency order. Two are already known and in scope:
- the 32x32 `part32` values under a superblock strip (stage 2 proper);
- `"a superblock-level HORZ/VERT strip with a split transform (per-unit rect
  prediction is not ported)"`.

For each: gate the case fires, implement, prove pixel-exact, lift the refusal and
update `refusal_inventory.rs` in the SAME commit. Declare every new refusal you
add — the inventory test fails the build otherwise.

r8's own class note: the same raw-vs-adjusted `tx_size` table indexing is worth a
grep sweep in anything touching transforms above 32. It checked the one sibling
site (`decode_block`'s plain 64x64 corner) and found it genuinely square, so
nothing else is owed right now — but if you add a size-indexed table lookup,
check which size libaom indexes it by.

## Budget discipline
75 turns, no reset on resume. At about turn 55, stop starting new work: commit
what is green and write your report.

Hard rules: `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-sbpart`; foreground
`nice -n 19 cargo ... -j4`; every `cargo test` a timeout >= 600000 ms;
`EC_AV1_REQUIRE_AOMENC=1`; aomenc `--threads=1 --row-mt=0 --sb-size=64`, and
this gate's recipe needs `--enable-tx-size-search=0`. Oracle rung 11 is yours and
already live. Sibling worktrees have live agents — never build in or edit them.
Never push, never merge into main; I handle merges. End with
`lanes/sbpart-r9.report.md`, VERDICT on line 1.
