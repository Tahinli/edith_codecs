# lane-sbpart r9 report

VERDICT: BLOCKED -- ranking delivered, fix attempted and reverted (pre-existing
defect discovered, not landed), working tree clean at d9a2f81 (no regression)

## Deliverable 1: the 25 refusals, ranked by firing count (40 attempts, seeds 42-81)

Gate: `a_real_aomenc_stream_with_a_superblock_level_horz_vert_partition_decodes_pixel_exact`
(`EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib
a_real_aomenc_stream_with_a_superblock_level_horz_vert_partition_decodes_pixel_exact
-- --nocapture --test-threads=1`), baseline result unchanged from r8: **15/40
pixel-exact, 25 named refusals, 0 mismatches, sb_rect_hits=36**.

| count | refusal | site |
|---|---|---|
| 13 | "a superblock-level partition type other than NONE or SPLIT (this decoder's intra tile path codes only those two at 64x64)" | `decode.rs:6756` -- SB-level (64x64) AB/HORZ_4/VERT_4 (`partition_w64` values 4-9), intra path only codes NONE/SPLIT/HORZ/VERT |
| 6 | "a partition below 16x16 other than a clean split (this decoder codes only the square arms below 16x16)" | `decode.rs:6479` |
| 3 | "a 32x32 partition type this decoder does not code (value=4)" | `decode.rs:6681` -- PARTITION_HORZ_A at 32x32 |
| 1 | "a 32x32 partition type this decoder does not code (value=6)" | `decode.rs:6681` -- PARTITION_VERT_A at 32x32 |
| 1 | "a HORZ/VERT intra strip in a screen-content frame (palette syntax is consumed for square blocks only)" | `decode.rs:2857` |
| 1 | "a Golomb tail longer than this decoder reads" | `decode.rs:1229` |

The charter's two named targets map onto this table as: the 32x32 `part32` AB
values are rows 3+4 above (4/25, values 4 and 6 only -- HORZ_B/VERT_B/HORZ_4/
VERT_4 never fired this run); "a superblock-level HORZ/VERT strip with a split
transform" (`decode.rs:3409`, tx_depth != 0 under `--enable-tx-size-search=0`)
fired **zero** times in this gate's 40 attempts -- the recipe's own flag makes
`tx_select` false for every attempt, so `depth` is always forced to 0 and that
branch is dead in this gate (still worth keeping as a refusal; a different
recipe with tx-size-search on would reach it, but this gate never proves or
disproves it). The dominant refusal by far (13/25, 52%) is row 1: AB/HORZ_4/
VERT_4 at the SB (64x64) level -- a bigger feature (four more partition types
at 64x64, all needing rect prediction plus rect residual, since HORZ_4/VERT_4
have no square-only decomposition) than either of the charter's two named
items. Order for a follow-on round: (1) row 1 once row-3/4's foundational
AB-geometry code exists and is proven (the 64x64 arms are the same four
shapes at double scale, reusing decode_block/decode_block_rect the same way);
(2) row 2 (a genuinely different capability -- HORZ_B/VERT_B *below* 16x16);
(3) rows 5/6 (1 hit each, lowest priority).

## Attempted: the 32x32 part32 AB values (rows 3+4)

Implemented `PARTITION_HORZ_A`/`PARTITION_HORZ_B`/`PARTITION_VERT_A`/
`PARTITION_VERT_B` arms in the part32 `match` (`decode.rs`, right before the
`_ =>` refusal at line ~6679), mirroring the already-merged, opus-verified
inter arms at the same partition values (`decode.rs:12062` `PARTITION_HORZ_A`
etc., lane-partab) but calling the intra primitives already proven pixel-exact
at this size: `decode_block` (16x16 squares, the same call `part16 ==
PARTITION_NONE` already makes) for the two square legs, `decode_block_rect`
(32x16/16x32, the same call the sibling `PARTITION_HORZ`/`PARTITION_VERT` part32
arms already make) for the strip leg. `PARTAB_HITS` (existing counter) reused
to prove firing.

Compiled clean. Gate run: **first attempt (seed 42) failed** --
`assert_eq!` on luma at frame 0, first mismatch at `px=91 py=0`
(`mi_row=0 mi_col=22`) -- inside the *first strip* (`decode_block_rect64`'s own
`PARTITION_VERT` arm for superblock 1, `mi_col=16`, `32x64`, **not** inside my
new AB code at all: this superblock's own `partition_w64` value was VERT, no
`part32` symbol was ever read for it).

## Root cause found (not this round's target, not fixed)

Range-ladder against the instrumented oracle
(`~/.cache/aom-oracle/build/aomdec`, `EC_TRACE_MODE=1 EC_TRACE_COEFF=1` on the
dumped mismatching stream, `EC_AV1_GATE_DUMP`) shows superblock 1's own
`PARTITION_VERT` split (two `decode_block_rect64` calls, `bw=32 bh=64`,
`mi_col=16` and `mi_col=24`) already diverges from ffmpeg's ground truth at
`mi_col=16`'s block, well before any AB code executes:

- oracle: strip 1 (`mi_col=16`) is a real all-zero-residual (`eob` implicit
  0) `DC_PRED` block -- flat 81 for its whole 32-wide span (px 64-95).
  ffmpeg's own decode matches this exactly.
- ours: strip 1 starts diverging from flat 81 at **px 91**, i.e. still inside
  its own bounds, 5 columns before strip 2 even begins at px 96 -- ours is not
  flat where the bitstream encodes a flat, all-zero-residual block.
- strip 2 (`mi_col=24`) has a real, small (`eob=6`, max level ~2) nonzero
  residual per the oracle trace; ffmpeg's decode is a smooth monotonic ramp
  81->116 with its visible onset around local column 15 of the strip. Ours
  produces a completely different curve (dip to 67, rise, a second dip) over
  roughly the same span -- not a shift, not a sign flip, a different shape
  entirely.

This is a **pre-existing defect in `decode_block_rect64`'s truncated-corner
reconstruction for a real (nonzero) TX_32X64 residual**, unrelated to my
diff -- superblock 1's own decode does not depend on anything later in the
frame, and `PARTAB_HITS` never incremented before this mismatch. It was
invisible before this round because in every one of the 25 refusal seeds, an
`unsupported(...)` fired *somewhere later* in the same frame's decode
(originally, in this exact seed 42, a `part32` value=4 refusal further into
the tile), and the gate's `Err(...) => { ... continue; }` arm skips the
frame comparison entirely on a named refusal -- so the mismatch this round's
fix exposed by letting the decode run further was always there, just never
reached. Class: **a removed refusal can uncover an unrelated defect that the
refusal was accidentally also shielding** (adjacent to, but distinct from,
[[refusal-lifted-without-a-gate]] -- this one *was* gated and *did* prove the
new capability's own geometry correct; the newly-visible defect is a sibling
function's, not the lifted refusal's own).

## Decision: reverted, not landed

Given the gate cannot go green until `decode_block_rect64`'s real-residual
truncated-corner reconstruction is fixed (a different, deeper defect than
either of this round's two named targets, and not something to hand-wave past
with a new refusal string wrapping *working, previously-merged* code), I
reverted the AB-partition decode arms (`git stash` + `git stash drop`) rather
than land a change that turns 25 clean refusals into red assertions. Working
tree is clean at `d9a2f81`, identical to the merge point -- **no commit this
round, no regression**.

## Not attempted

- SB-level AB/HORZ_4/VERT_4 (row 1, 13/25 hits) -- the biggest single item,
  correctly identified as next by the ranking, not started (budget went to
  the charter's own two named targets first, per instruction).
- The `decode_block_rect64` real-residual defect above -- found this round,
  not fixed; it blocks BOTH the part32 AB item (my attempt) and, by the same
  code path, any future SB-level AB/HORZ_4/VERT_4 work, since those would also
  produce 64x64-corner truncated blocks with real residuals through the same
  function. **This is now the actual highest-priority item for r10**, ahead of
  the ranking table above -- fixing it may also silently raise the 15/40 match
  rate on the *already-passing* HORZ/VERT cases that happen to have been
  lucky (all-zero or small enough residual to not show the bug).

## Hard rules followed

Worked only in this worktree; `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-sbpart`
every build; `nice -n 19 cargo ... -j4`; `EC_AV1_REQUIRE_AOMENC=1` on every
gate run; aomenc recipe unchanged (`--threads=1 --row-mt=0 --sb-size=64
--enable-tx-size-search=0`, inherited from the gate); read-only use of the
shared `~/.cache/aom-oracle` (already instrumented, no rebuild needed this
round); no other worktree touched; no push, no merge into main; nothing
committed (reverted before landing).
