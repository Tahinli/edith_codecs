# lane-realworld r6 — gate what r5 removed

At 15fe79a. Read `lanes/realworld-r5.report.md`.

## Where you stand
r5 finished both halves: `maybe_read_delta_q` / `maybe_read_delta_lf`, the
`CURRENT_Q_IDX` / `CURRENT_DELTA_LF` thread-locals, `Neighbours::delta_lf_grid`
written by `fill_lf_grid_rect`, and `lf_level` / `edge_params` applying the
per-block delta before ref/mode scaling (spec 7.14.4). Four green commits, suite
242/0 twice. Good round.

**It also removed the whole-frame refusal, and no fixture anywhere sets
`delta_lf_present`.** So the capability is code-verified against the spec's
shape and nothing more — the `DELTA_Q_HITS` / `DELTA_LF_HITS` counters exist but
have never been observed above zero. Until that gate exists, the removal is an
unproven claim and this branch does not merge to main. Writing it is job one;
nothing else in this lane matters until it is done.

If the gate cannot be made to fire, restore a narrow refusal for the half that
is unproven, in the same commit. That is the honest outcome, not a failure.

## Job 1 — the gate
Real aomenc stream, `--deltaq-mode=1` and/or `--aq-mode=1`, 128x64 or larger,
`--threads=1 --row-mt=0 --sb-size=64` (this decoder hardcodes 64px
superblocks), fixture through `gradients_source`, ffmpeg generate bounded with
`-t <seconds>`, written as an attempt loop that requires at least one decode
(main FAILS the suite if a gate turns a decode error into a printed SKIP).
Hard-assert `delta_q_hits() > 0` AND `delta_lf_hits() > 0`.

r5's own warning, worth taking seriously: verify against real aomenc output that
its CLI flags actually set `delta_lf_present` / `delta_lf_multi` — `--deltaq-mode`
may only ever set `delta_q_present`. If the encoder will not produce delta_lf at
all, say so with the command you ran, and refuse that half by name rather than
shipping it unproven. A per-superblock symbol also cannot fire in a
single-superblock fixture; size the frame accordingly.

## Then
Nothing else is assigned to this lane. If turns remain after the gate is green,
take `--enable-intrabc` — the last entry besides palette on main's
`gate_coverage.rs` `NEVER_EXERCISED` list — or say in the report that the lane
is clear.

## Merge note
Main is at eebbdd1 and now refuses `bit_depth != 8` by name: both AV1 films in
this box's library are 10-bit and reached your delta_q refusal, which is what
surfaced that `Picture`'s planes are `Vec<u8>`. Your branch removes the delta
refusal from `refusal_inventory.rs`; main has since added the bit-depth one and
reworded three partition refusals. Report every refusal string you add, remove
or reword, verbatim.

Hard rules: `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-realworld`; foreground
`nice -n 19 cargo ... -j4`; every `cargo test` a timeout >= 600000 ms. Sibling
worktrees have live agents — never build in or edit them. Never push, never
merge into `main`. 75-turn cap, does not reset: COMMIT AT EVERY GREEN STEP.
End with `lanes/realworld-r6.report.md`, VERDICT on line 1.
