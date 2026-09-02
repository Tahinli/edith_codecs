# lane-interp3 r3 -- the 8x8 COMPOUND leaf never applied the per-ref GLOBAL warp; gate is GREEN and un-ignored

Base: d06d93f (r2 handoff, already merged with main).

## Root cause (one line)

`decode_inter_block8`'s COMPOUND arm predicted **both** reference taps
translationally, while libaom applies `allow_warp`'s GLOBAL branch to every
`is_global_mv_block` reference slot -- compound included and independently of
`motion_mode` (`av1_init_warp_params` is called inside
`build_inter_predictors_8x8_and_bigger`'s per-`ref` loop, `reconinter.c:33-55`
for `allow_warp` itself). The 16x16+ compound arm has done this since
lane-cwarp r1 (`decode.rs:16691`); the 8x8 leaf is its twin and drifted.
Third instance in this lane of the class **twin functions drift**.

Effect on the pinned stream: the GLOBAL_GLOBALMV compound 8x8 leaf at
mi (10,4) of decode-order frame 7 (ref0=GOLDEN mv (0,0), ref1=ALTREF mv
(-3,4), `comp_group_idx=1` masked compound) was predicted with a pure
translation, so its prediction error was 0 at the block's top rows and grew
downward to |delta| 5 -- then propagated through every later frame.

## It was NOT the OBMC blend (the r2 handoff's first suspect, disproved)

An `EC_OBMC` emitter was added to both OBMC passes in `decode.rs`, printing the
instrumented aomdec's own byte format
(`EC_OBMC above|left mi=() wh=() rel= op= nbmv=() nbref= nbbsize= filt=`,
`decodeframe.c` `dec_build_prediction_by_{above,left}_pred`). The two decoders
emit **325 identical lines** on the pinned 16-frame stream -- neighbour
position, span, mv, reference, bsize and packed dual-filter pair all agree at
every OBMC call. The class sweep the charter asked for (neighbour-side fields
used by OBMC: filter, ref, mv, scale) is therefore CLEAN by measurement, not by
inspection; `obmc_mask_1/2/4/8/16/32` were also byte-compared against
`reconinter.c:752-765` and match.

Frame 7 has no OBMC block at mi (10,4) at all: it is a compound block, and
compound blocks take SIMPLE_TRANSLATION.

## Changed (path:line, this commit)

- `crates/ec-av1/src/decode.rs:19513` -- `leaf_compound_warp` +
  `warp0_c`/`warp1_c`, the leaf's per-slot copy of `decode_inter_block`'s
  `compound_warp` (`force_integer_mv` short-circuit, `gm.invalid` guard).
- `crates/ec-av1/src/decode.rs:19596,19618` -- `warp::warp_affine_compound`
  replaces each luma tap's translational intermediate, exactly as the 16x16+
  arm does at 17130/17155. Chroma is 4x4 at an 8x8 leaf, below
  `av1_init_warp_params`'s own per-plane 8px bound, so chroma stays
  translational (same rule the leaf's single-ref arm already applied).
- `crates/ec-av1/src/decode.rs:1786` -- `COMPOUND_WARP_HITS_8` +
  `compound_warp_hits_8()`: the gate's proof that the warp fired at an 8x8
  LEAF specifically (`COMPOUND_WARP_HITS` alone cannot tell it from a 16x16+).
- `crates/ec-av1/src/decode.rs:15806` (`ec_obmc_bsize`) and `15828`
  (`ec_obmc_trace`) + the two call sites in `obmc_blend` -- the `EC_OBMC` rung.
- `crates/ec-av1/src/decode.rs:14682,22658` -- `EC_PICT idx=` decode-order
  frame marker (printed under `EC_OBMC`/`EC_TRACE_MODE`), so a trace line can be
  attributed to the frame whose `EC_AV1_PREFILT_DUMP` file mismatched. Without
  it the r2 hunt mis-attributed every trace line by one frame.
- `crates/ec-av1/src/stream.rs:6332` -- the gate's `#[ignore]` removed, doc
  comment carries this round's measurement, and a third hard assert
  (`compound_warp_hits_8() > before`) added.

## Class sweep

Both inter block decoders are the only prediction sites
(`decode_inter_block` 16309, `decode_inter_block8` 19032); all 12
`predict_compound_intermediate` calls were audited. Warp coverage is now:
16x16+ single-ref (18235-18253), 16x16+ compound y/u/v x 2 refs
(17130-17300), 8x8 leaf single-ref (20238-20253), 8x8 leaf compound luma x 2
refs (19596/19618, chroma correctly excluded). No third twin exists.

## Gate: GREEN, un-ignored

`a_real_aomenc_dual_filter_obmc_8x8_inter_sequence_decodes_pixel_exact`
(`--enable-dual-filter=1 --enable-obmc=1 --enable-warped-motion=1
--enable-onesided-comp=1`, `--min-partition-size=8 --enable-ab-partitions=0
--enable-1to4-partitions=0`, 64x64, 16 frames). Four hard asserts now:
compound 8x8 leaf reading its own switchable filter, a block whose two
dual-filter directions DIFFER, an 8x8 OBMC blend, and an 8x8 compound leaf
predicted through the per-ref GLOBAL warp.

Command:
`EC_NOMEMGUARD=1 EC_AV1_REQUIRE_AOMENC=1 CARGO_TARGET_DIR=$HOME/.cache/cargo-target-interp3 cargo test -p ec-av1 --lib a_real_aomenc_dual_filter_obmc_8x8_inter_sequence_decodes_pixel_exact`
Result: `test result: ok. 1 passed; 0 failed` (log `$HOME/.cache/interp3-gate-r3.log`).

## Refusals

None lifted. The r1 refusal `an OBMC neighbour whose interp filter was never
recorded` stays (it does not fire on this stream); `refusal_inventory.rs` and
`gate_coverage.rs` unchanged.

## EVIDENCE

EVIDENCE: /tmp/claude-1000/-home-tahinli-Documents-Code-Rust-edith-codecs/b6d8a07f-96a4-4bbb-b378-af9ae25cf7c9/scratchpad/{aom.obmc,ours.obmc} | `EC_OBMC=1 aomdec --rawvideo -o /dev/null mm.obu` vs `EC_OBMC=1 decode_probe mm.obu`, `diff` of every EC_OBMC line | 325 vs 325 lines, diff EMPTY -- OBMC neighbour selection/mv/ref/bsize/filter exonerated
EVIDENCE: same dir, pf_a/f.f0..15 vs pf_o/f.f0..15 | `EC_AV1_PREFILT_DUMP` on both decoders, byte compare per decode-order frame BEFORE the fix | frames 0-6 identical, frame 7 35 bytes differ (all inside luma rows 42-47, cols 16-23 = the 8x8 leaf at mi (10,4)), growing to 224 at frame 15
EVIDENCE: same dir, pf_a vs pf_o2 | identical command AFTER the fix | all 16 decode-order frames byte-identical pre-filter (0 differing bytes)
EVIDENCE: $HOME/.cache/interp3-gate-r3.log | `cargo test -p ec-av1 --lib a_real_aomenc_dual_filter_obmc_8x8_inter_sequence_decodes_pixel_exact -- --nocapture` | `test result: ok. 1 passed; 0 failed` with all four counter asserts live (the run would panic if `compound_warp_hits_8` had not moved)
EVIDENCE: SUITE_LINE_PLACEHOLDER

## Residue

- deferred(lane-inter8's 10-bit 8x8-leaf desync) -- a 10-bit twin of this gate.
  Its sibling `a_real_aomenc_10bit_inter_sequence_with_an_8x8_leaf_split_...`
  is still `#[ignore]`d on an unrelated 10-bit desync (lane-inter8 r2), so a
  10-bit arm of this recipe would go RED on that, not on interp3's own path.
- deferred(needs a multi-tile recipe that reaches an 8x8 leaf) -- a
  `--tile-columns=1` arm. lane-inter8 r3 measured that the 128x64 multi-tile
  recipe stops at the SB-level rect-partition refusal before any 8x8 leaf.
