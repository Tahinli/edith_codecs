# lane-band46 r1 — the seed-46 "out-of-scope" mismatch: split transform units answered their intra reach as standalone blocks

## Premise, re-measured on main 146896f (not the chartered 8262b99)
`git log --oneline -1` in the worktree = `146896f` (lane-cfl merged after the charter was written).
The defect reproduces there, so it is a main defect, not a rectchroma-branch one.

Stream: the rectchroma gate recipe verbatim (192x128 bands + `noise=alls=10:all_seed=46`,
`--enable-ab-partitions=0 --enable-1to4-partitions=1 --enable-tx-size-search=1`, cq 32, 8-bit),
pinned twice: `sha256 9952fd31a1d51ccd5e066bd249babf676f6bb59318a1877272fa8fbd060d809b` both runs.

## Root cause — class `reach-is-per-transform-unit`
`crates/ec-av1/src/decode.rs` computed a split transform unit's intra reach with
`Reach::of(logical_tx, tu_px, tu_py, ..)` — the *standalone-block* `has_top_right`/
`has_bottom_left` table lookup, asking "what may a block of this size at this superblock
position read". libaom (`av1/common/reconintra.c`) never does that for a unit inside a block:
it answers from the unit's `row_off`/`col_off` within its parent, and only the (0,0) unit ever
reaches the table.

* `has_bottom_left`: `if (col_off > 0) return 0;` — the bottom-left samples of any unit but the
  leftmost column live in a unit that is not reconstructed yet.
* `has_bottom_left`, leftmost-superblock-column branch: counts `row_off + tx_size_high_unit`
  from the UNIT, not the block.
* `has_top_right`: `if (row_off > 0) return col_off + tx_size_wide_unit < plane_bw_unit;` — a
  lower row of units never consults the table at all.

Seed 46, frame 0: block at mi (4,0) is `D203_PRED` (mode symbol 7, angle_delta 0) with
`tx_depth=1`, i.e. four 8x8 units. Unit 1 (`col_off = 2`) got `below_left = true` from the
standalone 8x8 table; libaom gives 0. D203 is a zone-3 (left + bottom-left) predictor, so the
unit came out as a lower-right triangle of wrong samples that then propagated right across the
row of blocks.

## Changed
* `crates/ec-av1/src/encode.rs:1281` — new `Reach::of_tu(bw, bh, tx, row_off, col_off, px, py, w, h)`:
  libaom's per-unit ladder, delegating the (0,0) fallback to the existing square/rect block-level
  answers. `Reach` gained `PartialEq, Eq` (`encode.rs:1011`) so the counter below can compare.
* `crates/ec-av1/src/decode.rs:683` — `SPLIT_TU_REACH_FIX_HITS` + `split_tu_reach_fix_hits()` and
  the `tu_reach(..)` wrapper, which returns the per-TU answer and counts every unit where it
  DIFFERS from the old standalone answer (a hit == a unit whose reference samples used to be wrong).
* `crates/ec-av1/src/decode.rs` — three call sites rewired to `tu_reach(..)`:
  the multi-TU square-block loop (16x16+ blocks with `tx_depth > 0`), the 8x8 leaf's 2x2 4x4 grid,
  and the 4x8/8x4 leaf's depth-1 pair.
* `crates/ec-av1/src/stream.rs` — new gate `a_real_aomenc_band_stream_seed46_decodes_pixel_exact`.

## Class sweep (`grep -n "Reach::of(" crates/ec-av1/src/decode.rs`)
9 sites. 3 were the defect (fixed above). 1 (`decode.rs:4652`, the rect-strip per-TU loop) already
hand-rolls the correct libaom ladder inline — a previous lane found the same class there; left as
is, gated by its own strip gates. The remaining 5 (`7037`, `7499`, `7920`, `14881`, `16388`) and the
two `group_reach` sites are genuine BLOCK-level questions where the transform covers the block.

## Gate
`a_real_aomenc_band_stream_seed46_decodes_pixel_exact` (`crates/ec-av1/src/stream.rs`, end of file):
the sibling recipe, seeds 42..=51 on 8 AND 10 bit, every decoded stream compared to ffmpeg
plane by plane, non-`unsupported` errors fail, seed 46 is asserted to decode (never refuse) on
both depths, and `split_tu_reach_fix_hits` must be > 0 overall AND > 0 in a 10-bit pixel-exact
stream (his films are `yuv420p10le`).

```
EC_NOMEMGUARD=1 EC_AV1_REQUIRE_AOMENC=1 CARGO_TARGET_DIR=$HOME/.cache/cargo-target-band46 \
  cargo test -p ec-av1 --lib seed46 -- --nocapture
```
result: `ok. 1 passed` — `10 pixel-exact streams, 10 named refusals out of 20; 22 split transform
units took the per-TU reach (10 of them 10-bit)`. The 10 refusals are the sibling lane's open
1:4-chroma refusal, by name.

EVIDENCE: /home/tahinli/.cache/band46/s46_1.obu, ourspre.f0 vs aompre.f0 | regenerate seed 46 twice (sha256 identical), decode_probe vs `ffmpeg -f rawvideo` and EC_AV1_PREFILT_DUMP both decoders | BEFORE: 916 luma samples differ, first (x=14,y=18) ours 217 vs 208, bbox x 8..191 y 18..23, U and V exact, and the pre-loop-filter dump differs identically (913) so the defect is reconstruction
EVIDENCE: /home/tahinli/.cache/band46/ours.step vs aom.step | EC_TRACE_MODE_STEP both decoders, `diff` of the range ladders excluding tx_depth print lines | LADDER IDENTICAL, 147 vs 160 lines differing only in tx_depth *printing* (ours prints at 7 of 20 sites) => entropy decode exact, first divergent element = NONE
EVIDENCE: /home/tahinli/.cache/band46/ours.yuv vs ff.yuv | same decode after the fix | `cmp` PIXEL-EXACT (36864 bytes)

## Suite
`systemd-run --user --unit=band46-suite2-... -p MemoryMax=10G` running
`EC_NOMEMGUARD=1 EC_AV1_REQUIRE_AOMENC=1 CARGO_TARGET_DIR=$HOME/.cache/cargo-target-band46 cargo test -p ec-av1 --lib -j3`
-> `$HOME/.cache/band46-suite.log`: **ok. 323 passed; 0 failed; 30 ignored** (499.87s).
Sibling gates over the same tables (every `stream::tests::a_real_*` intra/strip/split gate,
`encode::tests::reach_matches_libaom_has_top_right_and_has_bottom_left`,
`rect_reach_tables_are_indexed_with_a_32_mi_row_stride`) are in that run and green.

`gate_coverage::tests::never_on_10bit_matches_the_gate_recipes` failed on the first suite run and
is fixed in the same commit: the new gate is the first 10-bit recipe to spell
`--enable-tx-size-search=1`, so `enable-tx-size-search` left `NEVER_ON_10BIT`
(`crates/ec-av1/src/gate_coverage.rs:283`). No refusal was lifted, so `refusal_inventory` is
unchanged (3 passed).

## Residue
* accepted: our `EC_TRACE_MODE_STEP` prints `tx_depth` at only 7 of the 20 sites aomdec prints
  (and prints `dq` value 0 where aomdec prints 128) — instrument coverage, not a decode
  difference; the ladder ranges match at every line.
* deferred(a lane owning the 1:4 chroma shapes): 10 of the 20 gate attempts still stop at
  "a coded HORZ/VERT strip whose chroma transform has no rect coefficient tables here"
  (lane-rectchroma's territory). The gate counts and prints them, never skips.
* accepted: `decode.rs:4652` keeps its inline copy of the per-TU ladder rather than calling
  `Reach::of_tu`; unifying them would re-open pinned rect-strip gates for no behavioural change.
