# lane-intersub8 r1 — HANDOFF (RED, code committed, gate red)

## 1. The refusal is REAL, not our desync (settled, do not re-litigate)
`aomenc` genuinely writes sub-8x8 partitions on INTER frames.
Recipe (192x128, 6 frames, gray ramp `128+58*sin((X+N*8)/6)+18*sin(Y/23)`, cq 8,
`--min-partition-size=4 --max-partition-size=16 --enable-rect-partitions=1
--enable-ab-partitions=0 --enable-1to4-partitions=0 --enable-obmc=0
--enable-tx-size-search=1 --sb-size=64 --lag-in-frames=0`), oracle:
`EC_TRACE=1 ~/.cache/aom-oracle/build/aomdec --rawvideo -o /dev/null u8_8.obu 2>ec.log`
`EC_PART_VAL bsize=3` histogram: **v0=68 NONE, v1=28 HORZ, v2=49 VERT, v3=3 SPLIT**.
So 8x4/4x8 dominate; 4x4 SPLIT is rare. `--enable-rect-partitions=0` removes
HORZ/VERT below 8x8 and still yields SPLIT groups (2 per stream at cq 8).

Side fact worth a sweep: the SAME trace shows `bsize=6 value=6/7` (VERT_A/VERT_B at
16x16) even though `--enable-ab-partitions=0` was on the command line — that is what
the "inter 16x16-level AB or 1:4 partition" refusal fires on for recipes with
`--min-partition-size=4`. Either the flag is not honoured at that size or the
refusal is itself hallucinated; lane-inter16ab should check with this trace.

## 2. What landed (compiles, `cargo check` clean)
* `crates/ec-av1/src/decode.rs` `decode_inter_sub8_split4` (new fn, above
  `decode_inter_block8`): four `BLOCK_4X4` inter sub-blocks of one 8x8 group.
  Symbols deliberately NOT read below 8x8, with libaom citations in the doc
  comment: `skip_mode` / `comp_mode` (`is_comp_ref_allowed`, blockd.h),
  `interintra` (`is_interintra_allowed_bsize`), `motion_mode`/`obmc`
  (`motion_mode_allowed`), tx size (`block_signals_txsize`, blockd.h:1027 —
  `bsize > BLOCK_4X4`, so `BLOCK_4X4` reads NO tx symbol even in TX_MODE_SELECT,
  decodeframe.c:1164-1177). Chroma once per 8x8 on the last sub-block, 4x4 TX,
  prediction = four 2x2 chroma pieces each from its own sub-block's mv/ref/filters
  (`dec_build_inter_predictors` is_sub8x8; spec 7.11.3.1). Chroma tx_type inherits
  the FIRST sub-block's luma tx_type (`av1_get_tx_type` co-located luma 0,0).
* call site: `decode.rs` ~21834, `part8 == PARTITION_SPLIT` dispatches; HORZ/VERT
  now refuse under a NARROWED string (rect sub-8x8 inter transform unimplemented).
* counter `SUB8_INTER_SPLIT_HITS` / `decode::sub8_inter_split_hits()`.
* `refusal_inventory.rs`: old string replaced by the 3 new ones.
* gate `stream.rs::a_real_aomenc_inter_sequence_with_a_sub8x8_inter_split_decodes_pixel_exact`.

## 3. Gate result: RED (two separate failures)
`cargo test -p ec-av1 --lib sub8x8_inter_split`
* 8-bit attempt 0 (cq 8, sp 3): **MISMATCHES with ZERO sub-8x8 inter split groups**
  → a defect this lane did not introduce and does not own (out-of-scope arm).
  Suspect: the oracle's 2 `bsize=3 value=3` in that stream are on the KEY frame
  (intra `decode_leaf_split4`), i.e. a pre-existing intra sub-8x8 defect on this
  recipe, OR our partition read desyncs before reaching them. CHECK FIRST: run the
  oracle trace on that exact stream and print the frame index of each b3 v3.
* 8-bit attempt 1 (cq 12, sp 3): 1 split group, luma **frame 5** mismatches.
  Frames 0..4 are exact, so the group is late in the sequence — bisect with
  `EC_TRACE_MODE` (`EC_MODE_VAL4` line added for the 4x4 sub-block) against the
  oracle's `EC_MODE`/`EC_MODE_VAL` at the same mi, comparing msac RANGE.
  Prime suspects, in order: (a) the sub-block `skip` context — I used the
  mi-granular `above_skip[cmi]`/`left_skip[rmi]` pair like `decode_inter_block8`,
  libaom uses `av1_get_skip_txfm_context`; (b) the chroma 2x2 piece offsets /
  which sub-block's filters each piece uses; (c) `record_mi_luma_rect` per 4x4 vs
  the group-level chroma neighbour tail; (d) `first_tx_type` when sub-block 0 is
  skip (libaom memsets tx_type_map to DCT_DCT for skip — we leave DctDct: OK).

## 4. Still refused after this round
`an inter 8x8 HORZ/VERT partition (8x4/4x8 inter leaves...)` — needs (1) INTER
TxbSet twins for 4x8/8x4 (`LumaRect4x8Inter`/`Set1`: side 8 tables +
`inter_tx_type_4`, copy `LumaRect4x8` and swap the tx_type slice), (2) a
rectangular var-tx read: for BLOCK_8X4/4X8, TX_MODE_SELECT + !skip reads exactly
ONE `txfm_partition` symbol (max_txsize_rect_lookup = TX_8X4/TX_4X8; split →
TX_4X4), our `read_block_tx_size`/`read_var_tx_size` are square-only.
That is the bulk of the remaining work and is why r1 scoped to SPLIT.
