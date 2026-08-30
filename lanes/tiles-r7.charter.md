# lane-tiles r7 — gate what r6 lifted, then the reach bound

At f9a0614. Read `lanes/tiles-r6.report.md`.

## Where you stand
r6 landed the inter per-tile loop (`decode_inter_frame_tile_with_cdfs` mirrors
the key-frame path) and bounded the MV-candidate scan by giving the shared
`MiGrid` tile bounds — one choke point instead of widening four
`find_mv_stack*` signatures. That was a good deviation from the charter's
literal wording and it stands.

**But it lifted the "an inter frame with more than one tile" refusal with no
gate proving a >1-tile-column INTER stream decodes.** r6 said so itself. Its
evidence is a clean `cargo check`, a non-regressing 242/0 suite and a 44/0
scoped subset — none of which exercise the capability the refusal used to
guard. Until the gate exists, that lift is an unproven claim, and this branch
does not merge to main. Nothing else in this lane matters until it is written.

## Order
1. The gate: a real aomenc >1-tile-column INTER stream, pixel-exact through
   `decode_stream`, with a HARD-asserted tile-hit count. Recipe:
   `--threads=1 --row-mt=0 --sb-size=64` (this decoder hardcodes 64px
   superblocks), two motion-carrying frames, fixture through
   `gradients_source`, ffmpeg generate bounded with `-t <seconds>`. Main FAILS
   the suite if a gate turns a decode error into a printed SKIP, so write it as
   an attempt loop requiring at least one decode. If the gate does NOT pass,
   restore the refusal in the same commit — that is the honest outcome, not a
   failure. COMMIT.
2. Re-run `a_real_aomenc_stream_with_two_tile_columns_decodes_pixel_exact` with
   `--loopfilter-control=0` REMOVED. `PlaneBuf`'s `tile_x1`/`tile_y1` clamps now
   receive real values at every call site, so this may already pass. If it does,
   lift BOTH the ">2 tile columns" and the "multi-tile + loop-filter" refusals
   in one commit — r5 established they are the same underlying reach-bound gap —
   and update `refusal_inventory.rs`. COMMIT.
3. Then tile rows, non-uniform spacing, several tile-group OBUs per frame, and
   `context_update_tile_id != 0` proven on a stream where it is not tile 0.

## Note
r5's handoff pointed at decode.rs:11661/11673 as unfinished; r6 found those were
already correct and the real break was two 2-arg `set_tile_origin` calls in the
key-frame loop. Stale pointers in a predecessor's handoff are worth one
`cargo check` before you trust them.

Merge note: main is at 53f5358. Report every refusal string you add, remove or
reword, verbatim, for `refusal_inventory.rs`.

Hard rules: `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-tiles`; foreground
`nice -n 19 cargo ... -j4`; every `cargo test` a timeout >= 600000 ms. Sibling
worktrees have live agents — never build in or edit them. Never push, never
merge into `main`. 75-turn cap, does not reset: land small, COMMIT AT EVERY
GREEN STEP — this lane has now spent four rounds partly on budget management.
End with `lanes/tiles-r7.report.md`, VERDICT on line 1.
