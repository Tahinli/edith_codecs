# lane-part32 r6 — the 16x16-level VERT_B arm was the one AB site r5 missed

Branch `lane-part32`, on top of r5 `02dbbd6`.

## Class sweep: every VERT_A/VERT_B site in decode.rs

`grep -n "PARTITION_VERT_A\|PARTITION_VERT_B" crates/ec-av1/src/decode.rs`, all
levels, intra and inter:

| site | level | path | guard at r5 |
|---|---|---|---|
| `decode.rs:7008` | 16 | intra VERT_B (8x16 rect + TR/BR 8x8 `decode_leaf8`) | **MISSING — fixed this round** |
| `decode.rs:7538/7612` | 32 | intra VERT_A / VERT_B | present (r5) |
| `decode.rs:7779` | 64 (SB) | AB arm, vert arms only | present (r5) |
| `decode.rs:13445/13606` | 32 | inter VERT_A / VERT_B | present (r5) |

No other site exists: the 16-level inter path has no AB arms (they are refused by
name, "a HORZ_A/HORZ_B/VERT_A partition below 16x16"), and 8-level partitions
below that are SPLIT-only. So one missed site, now guarded — the class is closed
for VERT_A/_B.

The verifier's computation is confirmed exactly (checked bit by bit against the
transcribed tables in `encode.rs`): `has_bl_8x8` reads **0** where
`has_bl_vert_8x8` reads **1** at all 16 even-8x8-row / odd-8x8-column slots of a
superblock — that slot is precisely the TR square of a VERT_A/_B — so the TR
square's `below_left` was wrongly false for the bottom-left-reading directional
modes (angle > 180: `D203_PRED`, `D157_PRED` with a positive angle delta).
Mirror-image for `has_tr_8x8` vs `has_tr_vert_8x8` (1 -> 0) at odd-row /
even-column slots, which VERT_A/_B never visit at 8x8 (no BL square in either).

## Files changed

- `crates/ec-av1/src/decode.rs:7010` — the intra 16-level `PARTITION_VERT_B` arm
  takes `crate::encode::Reach::vert_ab_partition()` for the whole arm; the left
  8x16 rect is unaffected by design (`Reach::of_rect`, libaom's own comment says
  vertical rectangles keep the non-vert table), the two `decode_leaf8` squares
  now read the vert tables.
- `crates/ec-av1/src/encode.rs:3565` — `vert_ab_partition_flips_below_left_for_the_top_right_8x8`,
  a table pin next to the existing libaom-availability test: all 16 TR slots flip
  `below_left` false -> true under the guard, every other slot is untouched, and
  the guard does not leak past its scope.

## Gate: attempted, could not be built — the fix stays source-verified

The charter's stream gate (a real-aomenc 16-level VERT_B, directional modes on,
decoded and compared pixel-exact) was written and run, and it cannot fire on this
aomenc build. Bounded sweep, all with
`--min-partition-size=8 --max-partition-size=16 --enable-rect-partitions=1
--enable-1to4-partitions=0 --enable-directional-intra=1 --enable-angle-delta=1
--cpu-used=0 --kf-max-dist=0 --passes=1`, 64x64 grey mandelbrot key frames:

| recipe | attempts | decoded | refusals |
|---|---|---|---|
| cq=45, `--enable-ab-partitions=1` | 40 | 0 | 40 (below-16 HORZ_A/HORZ_B/VERT_A, or a coded non-skip rect strip below 16) |
| cq=45, ab={0,1} | 10 + 10 | 0 | 8 AB-below-16 + 2 coded-rect-strip, each |
| cq=63, ab={0,1} | 40 + 10 | all | none — but `partition_w16` never resolves to VERT_B (value=7): the frame stays at 16x16 NONE |
| cq={55,57,60}, mandelbrot / testsrc2 / cellauto | 12 | 3 | same two below-16 refusals |

`EC_AV1_TRACE=1` `partition_w16` value histogram over every decoded stream: 0
occurrences of value=7 at any quality that decodes. Every stream that partitions
small enough to *reach* a 16-level VERT_B hits one of the two still-refused
below-16 capabilities first (HORZ_A/HORZ_B/VERT_A-below-16, coded rect strip
below-16), which abort the tile before the VERT_B arm. Writing a gate that
decodes only cq=63 streams would have been vacuous
(`gate-blind-to-feature`), so the sweep gate was **removed** rather than
committed green, and the pin above carries the fix instead.

EVIDENCE: /tmp/.../scratchpad (r6 sweep, shell loop above) | 40+40+20+12 real aomenc encodes, `EC_AV1_TRACE=1 decode_probe` on each | 0 streams with `partition_w16 value=7`; at cq=45 40/40 refused with the two named below-16 refusals, at cq=63 50/50 decoded with 0 VERT_B

EVIDENCE: `cargo test -p ec-av1 --lib -- vert_ab_partition_flips` | table pin of the guard at all 64 8x8 slots of an interior superblock | `test result: ok. 1 passed`; 16 TR slots flip false->true, 48 unchanged

EVIDENCE: /tmp/.../scratchpad/r6full.txt | `EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib` | `test result: ok. 270 passed; 0 failed; 24 ignored; 0 measured` in 1585s (269 before this round's new pin)

## Refusals

None lifted or added this round (the two below-16 refusals above are other
lanes'). `refusal_inventory` and `gate_coverage` unchanged and green in the run
above.

## Residue

- deferred(a lane that ports HORZ_A/HORZ_B/VERT_A below 16x16, or the coded
  non-skip rect strip below 16x16): the pixel-exact stream gate for the 16-level
  VERT_B arm. Both refusals fire before the arm on every recipe swept; once
  either lands, re-run the sweep in this report and the gate becomes writable
  as-is.
- accepted: the fix is source-verified + table-pinned, not stream-proven. It is a
  one-bit availability change of exactly the shape r5 proved by pixel comparison
  one level up (4683 wrong luma samples there).
