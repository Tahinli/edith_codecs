# lane-av1422sub8pix — the sub-8 4:2:2 chroma pixel defect fixed at source: the
# odd leaf's CfL source stepped into the right neighbour group and the inter
# piece's left half read the reference's U plane for BOTH chroma planes; frame-1
# U/V at the named mi(0,22) site are now exact and the f15 postcdef luma
# knock-on is gone

## Outcome

1. **Reproduction at HEAD c5d88f0b.** The mi24 lane's named residual reproduced
   exactly on the pinned t422 fixture (`/tmp/i422/t422_2f.obu`): frame 0 exact
   at prefilt; frame 1 Y exact, U first diff chroma (4,44) with rows 0..3
   exact / rows 4..7 wrong across cols 44..47, V first diff (0,44).
2. **Localization (EC_PRED/EC_PREDOUT8 + EC_DQCOEFF pairing vs the oracle).**
   For the frame-1 8x8 SPLIT group at mi(0,22) — leaves (0,22)/(0,23) inter,
   (1,22) inter with luma residual, (1,23) INTRA (filter-intra, uv_mode
   UV_CFL_PRED) — the oracle writes chroma rows 0..3 from leaf (0,23)'s
   `build_inter_predictors_sub8x8` (row-0 steps only; `ss_size_lookup
   [BLOCK_4X4][1][0]` is BLOCK_4X4, so the loop covers b8_h=4) and rows 4..7
   from intra leaf (1,23)'s floored-anchor TX_4X4 unit. Both sides' unit
   prediction (DC base 97, `EC_PREDOUT8`/`OUR_PRED` sum 1552) and dequantized
   coefficient grid (`EC_DQCOEFF` vs `OUR_DQ`: −152/+90/−180/+270/−90/+90,
   raster) are IDENTICAL — the defect is neither entropy nor placement.
3. **Root cause 1, class `sub8-cfl-source-anchor`** (`sub8_leaf_chroma422`):
   the CFL AC signal was sourced at the LEAF's own luma x (`px` = 92 for leaf
   (1,23)) instead of the FLOORED unit anchor (88) the same function already
   uses for the write/prediction anchor — the source averaged luma 92..99,
   i.e. half the span from the RIGHT neighbour group. Fixed: the CFL source
   anchors at `(lmi.1 & !1) * MI`, the same floor as the chroma write anchor
   (libaom `cfl_store_tx`/`sub8x8_adjust_offset` store the group's luma; the
   unit's AC span is the group's own 8-wide band at the leaf's own rows).
4. **Root cause 2, class `sub8-left-piece-wrong-plane`**
   (`decode_inter_sub8_split4`'s 4:2:2 inter piece arm): the left 2x4 half of
   each piece's chroma prediction (`is_sub8x8_inter`'s `mi[-1]` step) grabbed
   `(_, l_ref_u, _)` from the left piece's reference planes and fed that ONE
   plane to both U and V iterations — the V plane's left half predicted from
   the reference's U samples (U exact, V wrong, matching the residual's V-only
   left-half pattern). Fixed: the loop enumerates U/V and picks the left
   piece's matching plane.
5. **Both edits are ss (1,0)-only code** — `sub8_leaf_chroma422` is reachable
   only from the 4:2:2 sub-8 arms, and the inter piece fix sits inside the
   `chroma_422 && (cmi & 1) == 1` arm — 4:2:0/4:4:4 never execute it.

## Verification

- **t422 16-frame ladder** (decode_probe + all three stage dumps vs the
  oracle's, our dumps /tmp/i422/sub8pix16/*):
  * **frame 0: EXACT at prefilt, postdeb AND postcdef** (Y, U, V all 0);
  * **LUMA: 0 diffs on EVERY frame 1–15 at prefilt, postdeb AND postcdef** —
    the mi24 lane's f15 postcdef Y=5059 CDEF knock-on no longer reproduces
    (f15 postcdef Y=0; oracle-side f15 postcdef dump absent from the ladder
    dir, so f15 postcdef is claimed at prefilt/postdeb only);
  * chroma frame 1: U 4175 → 3837, V 4636 → 3879; the named sites are pixel
    exact: U rows 4..7 cols 44..47 and V rows 0..3 cols 44..45 verified
    sample-exact vs the oracle; chroma improved on both planes at every frame.
- **Frame exactness claimed ONLY where measured 0:** frame 0 all stages;
  frames 1–15 LUMA at prefilt/postdeb (postcdef too, minus f15 where the
  oracle dump is missing). Frames 1+ chroma are NOT claimed.
- **The residual, site narrowed (next lane's first diff).** Frame-1 prefilt
  chroma now first diffs at U (32,87) / V (32,80) — the 8x8 inter SQUARE
  blocks mi(8,40)/(mi(8,42) for U) (PART 16x16→8x8→NONE, one TX_4X8 chroma
  unit per plane, flat prediction per the oracle's EC_PREDOUT8): ±1-class
  diffs concentrated in each block's rightmost chroma column — the 4:2:2
  square-8x8 chroma MC class (`chroma422_square` territory), NOT sub-8.
- **4:2:0/4:4:4 identity:** the lossless gates stay green —
  `a_lossless_libaom_key_frame_decodes_sample_exact`,
  `a_lossless_libaom_inter_frame_decodes_sample_exact`,
  `a_real_aomenc_mixed_lossless_segment_frame_decodes_sample_exact`.
- `cargo check -p ec-av1` (target dir `~/.cache/cargo-target-av1422sub8pix`):
  **0 warnings, 0 errors**.
- The `EC_AV1_ALLOW_422_PROBE` bypass was applied to stream.rs for the probe
  runs and REVERTED; `git status` names only decode.rs (+ this report).
- The intra-leaf evidence used local-only `EC_DBG_RES` write watches in
  decode.rs; all three were removed before this commit (no trace remains).

## State

- Commit on lane-av1-422sub8pix (no push): the two 4:2:2 sub-8 chroma
  prediction fixes in `crates/ec-av1/src/decode.rs` + this report.
- Next stop named for the owning lane: the 4:2:2 square-8x8 inter chroma
  class — first sites frame-1 U (32,87) (block mi(8,42)) and V (32,80) (block
  mi(8,40)), ±1 diffs hugging each block's right chroma column with the
  oracle's own prediction flat and matched at the EC_PREDOUT8 rung — MC
  reference-edge territory for the 4-wide chroma unit, not sub-8.
