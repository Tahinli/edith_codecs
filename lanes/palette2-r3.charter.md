# lane-palette2 r3 — palette on a rectangular strip

At main (71a2624; your UV work is merged and pushed — the branch is
fast-forwarded, nothing to merge). Read `lanes/palette2-r1.charter.md` for the
lane's full scope and `lanes/palette2-r2.report.md` for what just landed.

Step 1 (UV palette reconstruction) is CLOSED: gated, pixel-exact, on main.
This round is step 2, and the measurement behind it stands — `smptebars` and
`rgbtestsrc` at plain default aomenc settings both stop here:

    "a HORZ/VERT intra strip in a screen-content frame
     (palette syntax is consumed for square blocks only)"

r2's handoff points at `decode.rs` ~9198-9202 and ~9263-9266 (line numbers will
have shifted by the merge; grep the string).

`palette_bsize_ctx` and the palette size/context derivation assume a square
block. libaom derives both from the block's width AND height — check what it
actually does for a rect block before adapting the square path, and note that a
narrowed table breaks the moment its indexing field moves
([[cdf-row-held-constant]]).

Order, as always: gate FIRST below `decode_stream`, hard-asserting the rect-strip
palette path fired ([[gate-blind-to-feature]]); then the implementation; then
lift the refusal and update `refusal_inventory.rs` in the SAME commit. The UV
refusal string stays in the inventory either way — it is still live at the
inter-frame leaf site.

If step 2 lands with budget to spare, step 3 is "a palette block with a split
luma transform" (`life` reaches it). Do not start it before step 2 is committed
green.

## Budget discipline
75 turns, no reset on resume. At about turn 55, stop starting new work: commit
what is green and write your report.

Hard rules: `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-palette2`; foreground
`nice -n 19 cargo ... -j4`; every `cargo test` a timeout >= 600000 ms; ffmpeg
generates bounded with `-t`; `EC_AV1_REQUIRE_AOMENC=1`; aomenc
`--threads=1 --row-mt=0 --sb-size=64`. Oracle rung 12 is yours; the oracle is
SHARED, env-gated rungs only. Sibling worktrees have live agents — never build in
or edit them. Never push, never merge into main; I handle merges. End with
`lanes/palette2-r3.report.md`, VERDICT on line 1.
