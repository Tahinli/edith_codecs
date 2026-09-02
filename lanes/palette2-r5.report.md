VERDICT: BLOCKED — gate written and hard-asserting, but real aomenc RD never lands a rect-strip
palette block across the whole sweep; r4's implementation is neither confirmed nor refuted this
round, and the refusal is NOT lifted.

## What I did
Wrote `a_real_aomenc_stream_with_rect_palette_decodes_pixel_exact` in
`crates/ec-av1/src/stream.rs` (after the existing square palette-Y/UV gates). It drives
`decode_stream` directly (below any refusal), hard-asserts `decode::palette_rect_hits()` moved
before comparing any pixels, and requires pixel-exactness vs `ffmpeg` on any attempt that both
decoded and fired the counter — exactly the charter's shape.

## What the gate found, measured live this round
`smptebars`/`rgbtestsrc` at plain default aomenc settings (r1's measured recipe) do NOT reach a
clean rect-strip palette decode: every attempt hits an unrelated named refusal FIRST, almost
always a 32x32-level `HORZ_A`/`HORZ_B`/`VERT_A`/`VERT_B` partition (this decoder codes only
`NONE`/`SPLIT`/`HORZ`/`VERT` at that level) or filter-intra-on-a-strip. Confirmed live that
`--enable-ab-partitions=0` is INERT for this content/tune combo — two encodes differing only in
that flag produced byte-identical output (`md5sum` match) — the same aomenc quirk already
recorded in the ledger for SB-level AB
([[lane-sbpart-r2]]: "AB-at-64 not gated by that flag"), here reproduced one level down at 32x32.

Forcing `--min-partition-size=64 --max-partition-size=64` sidesteps that 32x32-level code path
entirely (only `NONE`/`SPLIT`/`HORZ`/`VERT`/AB/1:4 remain, straight from `decode_block_rect64`),
and does produce many clean `OK` decodes across a 70-attempt sweep (2 sources x 5 sizes x 7 cq
levels, single key frame each, `--enable-palette=1`). But `palette_rect_hits` stayed at its
`before` value on every single one of them — the encoder's RD always prefers `PARTITION_NONE`
(whole-SB square palette, already gated by the existing `palette_y`/`palette_uv` tests) over a
`HORZ`/`VERT` split when palette is in play at this scale. The remaining ~31 attempts hit named
refusals, dominated by "a palette block with a split luma transform (round 1)" — r1's own
already-refused square case, not this round's.

So the gate is real, hard, and currently RED for the right reason: it never got to compare a
single pixel because the capability it is meant to prove never actually fires against real
content, not because of a code defect it caught. This is different from a "mismatch to bisect"
outcome the charter anticipated — there is nothing to range-ladder yet because nothing decoded
through the rect-palette path to disagree with ffmpeg on.

## Why I did not lift the refusal
The charter is explicit: "Only with the gate green: lift the refusal." The gate is not green — it
never observed a genuine attempt. Lifting on an unfired gate would be exactly the "refusal lifted
without a gate" class this project forbids. r4's `read_intra_mode_rect`/`decode_block_rect64`
palette wiring (the diff in 05f6db8) is therefore STILL unverified — neither confirmed correct nor
caught wrong. I left it in place (it compiles, does not regress any other gate) and left r4's
`refusal_inventory.rs` edit (narrowing the refusal to "palette block with a real transform on a
rect strip") as-is, also still unverified, per the charter's own note to treat it that way.

## State committed
- `crates/ec-av1/src/stream.rs`: the new gate test only. No other file touched.
- `refusal_inventory.rs`: unchanged from r4's commit (still unverified, not touched further).
- The suite has one new RED test (`a_real_aomenc_stream_with_rect_palette_decodes_pixel_exact`).
  I did not silence or ignore it — a red gate that has never fired is the honest state, not a
  green one that never proved anything.

## Next round should try
- Content specifically constructed to make a `PARTITION_HORZ`/`VERT` split cheaper than `NONE`
  for a palette block — e.g. two very different flat-colour halves stacked so the whole-SB
  palette needs 2x the base colours a half-SB one would, biasing RD toward the split. Plain
  `smptebars`/`rgbtestsrc` don't do this (their bands are much finer than a 64px SB).
  `gradients_source` (used by the free-partition gate) is NOT screen content and won't turn on
  palette at all.
  - `aomenc --partition-info-path` (seen in `--help`) may let a fixture force a specific partition
    outright rather than depending on RD luck — worth trying before another blind cq/size sweep.
- If a firing stream is found and it MISMATCHES ffmpeg, rung 12 of the range ladder is still open
  and unclaimed — use it then.

## Commands run
`export CARGO_TARGET_DIR=$HOME/.cache/cargo-target-palette2; export EC_AV1_REQUIRE_AOMENC=1`
`nice -n 19 cargo test -p ec-av1 --lib -j4 a_real_aomenc_stream_with_rect_palette_decodes_pixel_exact -- --nocapture`
(timeout 600000ms; result: 1 failed, as reported above — 0 passed, 31 named refusals, 39 decoded
with `palette_rect_hits` unchanged, 0 matched out of 70 attempts)

## Budget
Spent the bulk of the round on live aomenc probing (`decode_probe` example + manual `aomenc`
sweeps) to characterise exactly where real content stops, per the charter's own instruction to let
the gate (and probes) tell the truth rather than guess flags. Stopped adding new sweep dimensions
at turn ~55 per budget discipline; committing the gate as the round's deliverable.
