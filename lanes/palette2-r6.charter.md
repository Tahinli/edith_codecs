# lane-palette2 r6 — gate the case that actually occurs

At the r5 commit. Read `lanes/palette2-r5.report.md`. r5 did good work and
reported an honest RED: its gate hard-asserts `palette_rect_hits()` and 70
aomenc attempts never moved that counter, so r4's implementation is still
unverified.

## What I think went wrong, from my own measurement
The counter is too narrow. The refusal r4 removed was:

    "a HORZ/VERT intra strip in a screen-content frame
     (palette syntax is consumed for square blocks only)"

That fires on a rect strip in a frame with screen-content tools enabled —
whether or not that particular block ends up USING a palette. r5 searched for a
block that both is a rect strip and codes a palette, which RD apparently almost
never picks. But the broad case is common: **I hit that exact refusal on main
with plain default settings**, no flag hunting:

    ffmpeg -v error -f lavfi -i "smptebars=size=192x128:rate=30" -t 0.2 \
      -pix_fmt yuv420p in.y4m -y
    aomenc --codec=av1 --ivf --threads=1 --row-mt=0 --sb-size=64 --limit=3 \
      --cpu-used=4 --end-usage=q --cq-level=32 -o out.ivf in.y4m
    ffmpeg -v error -i out.ivf -c:v copy -f obu out.obu -y
    cargo run -p ec-av1 --example decode_probe -- out.obu

On main that prints the refusal above. `rgbtestsrc` does the same. On THIS
branch the refusal is gone, so the same stream now runs through r4's code — and
that is the thing to gate.

## The job
1. Reproduce the above on main first, so you are gating a case you have watched
   occur rather than one you hope occurs.
2. Add a counter at the site r4 actually changed — the rect-strip-in-a-
   screen-content-frame path — not a palette-specific one. Then gate: this
   stream, through `decode_stream`, pixel-exact vs ffmpeg, with that counter
   hard-asserted to have moved.
3. If pixels match, the lift is earned: keep r4's narrower refusal
   ("a palette block with a real transform on a superblock-level HORZ/VERT
   strip...") and update `refusal_inventory.rs` so the lists agree.
4. If pixels mismatch, that is r4's implementation being wrong — bisect with a
   range ladder against the oracle (rung 12 is yours) and say so.

**Right now this branch has removed a refusal with no gate behind it**
([[refusal-lifted-without-a-gate]]), which is the one state that cannot merge.
Closing that is this round's whole point.

Keep r5's gate too if you can make it fire, but do not spend the round on it —
its own report says the honest next lever is `aomenc --partition-info-path` or a
fixture with two very different flat-colour halves, and that is a later problem.

## Budget discipline
75 turns, no reset on resume. At about turn 55, stop starting new work: commit
what you have — red included, named as such — and write your report. Landing red
on this branch is fine; only merging red is forbidden, and I do the merges.

Hard rules: `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-palette2`; foreground
`nice -n 19 cargo ... -j4`; every `cargo test` a timeout >= 600000 ms; ffmpeg
generates bounded with `-t`; `EC_AV1_REQUIRE_AOMENC=1`; aomenc
`--threads=1 --row-mt=0 --sb-size=64`. The oracle is SHARED — env-gated rungs
only. Sibling worktrees have live agents — never build in or edit them. Never
push, never merge into main. End with `lanes/palette2-r6.report.md`, VERDICT on
line 1.
