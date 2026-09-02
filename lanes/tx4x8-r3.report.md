# lane-tx4x8 r3 -- every rect shape gets its own has_tr/has_bl row

## Root cause (verifier's finding, confirmed)

`encode.rs` `rect_reach_tables(bw, bh)` had rows for 4x8, 8x4 and 16x32 only and
a `_` arm that routed **every other rectangular shape** to libaom's
`has_tr_32x16`/`has_bl_32x16`. 16x8 strips are live (`decode_leaf_rect`,
`decode.rs`), and neither the old `HAS_TOP_RIGHT_RECT[bw == 16]` selection nor
the r2 `_` arm matches `has_tr_16x8`/`has_bl_16x8` (`reconintra.c:96` / `:282`,
16 bytes each): over the 21 reachable superblock positions, has_tr was wrong at
14 (old) / 10 (r2) and has_bl at 14 / 7. Class:
[[enumerate-table-domain]] + [[tool-disabled-in-every-gate]].

## Fix

* `encode.rs`: all 14 rect shapes this decoder codes or will code (4x8, 8x4,
  8x16, 16x8, 16x32, 32x16, 32x64, 64x32, 4x16, 16x4, 8x32, 32x8, 16x64, 64x16)
  now have their own `HAS_TR_*`/`HAS_BL_*` const, **generated** from
  `~/.cache/aom-oracle/src/av1/common/reconintra.c` (no byte retyped), and
  `rect_reach_tables` is an exhaustive match ending in
  `unreachable!("no libaom has_tr/has_bl table for a {bw}x{bh} block")`.
* Index rule verified per shape: libaom's
  `(blk_row_in_sb << (5 - bw_in_mi_log2)) + blk_col_in_sb` == row stride
  `128 / bw` == the existing `Reach::table_stride(bw)`. For a 64-pixel
  superblock the largest index any shape can produce is
  `(64/bh - 1) * (128/bw) + (64/bw - 1)`, which is inside every one of the 14
  oracle tables -- so the `% (table.len() * 8)` wrap the rect path used to
  apply is not just unnecessary, it was masking exactly this kind of
  mis-routing; it is gone (the square path keeps its own).

## Check 1 -- domain test (runnable, green)

`encode::tests::every_rect_shape_reaches_what_libaom_says_over_the_whole_superblock`:
for all 14 shapes, at every position a block of that shape can sit in a 64x64
superblock (and in a second superblock at (64,64), so SB-relative position is
what decides), `Reach::of_rect` == a port of libaom's `has_top_right` /
`has_bottom_left` bodies incl. every early exit (top/right/bottom/left
availability, `blk_row_in_sb == 0`, rightmost column, leftmost column's
`row_off_in_sb + count < sb_height_unit`, bottom row). Each shape's table is
additionally pinned by a `(len, byte sum)` fingerprint taken from the oracle's
arrays, so a re-routed or truncated row fails even where two rows agree.

Not covered, stated rather than implied: chroma `ss_x`/`ss_y` and sub-block
transforms (`row_off`/`col_off` > 0) -- `of_rect` takes neither parameter
(callers predict each transform unit as its own block); `PARTITION_VERT_A/B`
needs no arm because libaom's `has_tr_vert_tables` entry for every *rect* bsize
is either the plain table (4x8, 8x16, 16x32, 32x64) or NULL.

    cd /home/tahinli/Documents/Code/Rust/edith_codecs-tx4x8
    CARGO_TARGET_DIR=$HOME/.cache/cargo-target-tx4x8 EC_NOMEMGUARD=1 \
      cargo test -p ec-av1 --lib -j3 every_rect_shape

EVIDENCE: $HOME/.cache/tx4x8-suite-r3.log | cargo test -p ec-av1 --lib every_rect_shape | 1 passed, 0 failed

MUTATION (the check that says the test has teeth): restoring r2's routing for
one shape -- `(16, 8) => (&HAS_TR_32X16, &HAS_BL_32X16)` -- fails it:

    assertion `left == right` failed: rect_reach_tables(16, 8) is not libaom's has_tr_16x8/has_bl_16x8
      left: (4, 32, 4, 184)
     right: (16, 960, 16, 2400)

## Check 2 -- the asymmetric strip stream gate: BLOCKED, `#[ignore]`d

`stream::tests::a_directional_16x8_strip_reads_the_right_above_right_samples`
is written in full (directional-only intra: `--enable-smooth-intra=0
--enable-paeth-intra=0 --enable-filter-intra=0 --enable-directional-intra=1
--enable-rect-partitions=1 --enable-ab-partitions=0`, 64x16 fixture whose
bottom half is flat so a HORZ arm can be a skip strip, 8-bit and 10-bit arms,
per-attempt counters `decode::rect_strip_pred_hits` / `rect_strip_reach_hits`
recorded in the predictor's edge fetch, pixel-exact vs ffmpeg) but it cannot
pass today and is `#[ignore]`d with the reason in its doc rather than skipping
from inside ([[gate-skips-on-its-own-failure]]).

Why: the 16x16-level strip decoder refuses every **non-skip** 16x8/8x16 strip
("a coded (non-skip) HORZ/VERT rect strip below 16x16"), which is what real
streams contain. Measured on this recipe, per 40 attempts:

| recipe | outcome |
| --- | --- |
| `--min-partition-size=16 --max-partition-size=16` | 40/40 decode, **0** 16x8/8x16 strips (aomenc bounds a rect shape by its smaller side, so 16x8 is never searched) |
| `--min-partition-size=8`, cq 8..20, noise 30, 64x64 | 40/40 refused (split-transform strip / non-skip strip / sub-16x16 AB) |
| `--min-partition-size=8 --enable-tx-size-search=0`, cq 30/45/55/62, noise 4 and 15, 64x16 | 0 firing; at cq 45 with the half-flat fixture: **38/40 "coded (non-skip) rect strip below 16x16"**, 2/40 sub-16x16 AB |

MUTATION for this gate is therefore not reportable at stream level: with the
old `_` routing the gate fails for the same reason it fails with the fix (no
attempt gets past the refusal). The mutation evidence for the fix is the
domain-test one above.

deferred: the stream-level pixel gate for 16x8/8x16 reach -- blocked by the
live "coded (non-skip) HORZ/VERT rect strip below 16x16" refusal -- unblocked
by the lane that ports the coded 16x16-level strip; then delete the
`#[ignore]` attribute (nothing else in the test changes).

## Also

* `stream.rs` doc of the sub-8x8 rect gate said "Arms: 8-bit 64x64, ... 128x64,
  ... 10-bit 64x64"; the arms are 16x16 / 128x16 / 16x16. Fixed.
* New counters `rect_strip_pred_hits` / `rect_strip_reach_hits` in `decode.rs`
  (bumped in `Plane::edges_rect`) are the instrument the blocked gate needs the
  day the refusal lifts.

## Suite

    CARGO_TARGET_DIR=$HOME/.cache/cargo-target-tx4x8 EC_NOMEMGUARD=1 \
      EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 -j3

275 passed, 0 failed, 23 ignored (r2: 274 / 0 / 22 -- +1 = the domain test,
+1 ignored = the blocked gate). Log: `$HOME/.cache/tx4x8-suite-r3.log`.

## Merge note

This round deletes `HAS_TOP_RIGHT_RECT` / `HAS_BOTTOM_LEFT_RECT` (the 2-row
arrays lane-tx64x16 r3 extends to 4 rows with its own 32x8/8x32 entries). On a
merge, keep this branch's per-shape consts -- `HAS_TR_32X8` / `HAS_BL_32X8` /
`HAS_TR_8X32` / `HAS_BL_8X32` are already here, byte-identical to the oracle --
and drop the array form together with `Reach::rect_table`.
