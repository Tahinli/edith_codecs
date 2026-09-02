# lane-intrasplit r2 HANDOFF (tip 6e883c9, branch lane-intrasplit)

## Merge state (all committed on this branch)
main 48b35ab -> lane-sqdrift 7d498c0 -> lane-r14 b86eb38 -> lane-inter16ab 4811488 (tip
moved from the charter's af7de9f) -> lane-intra14 f4b5419 (was 8313c4a). cdf.rs byte-equal
to main. Conflict resolutions: stream.rs (three gates interleaved in ONE hunk — all three
kept), decode.rs (both thread-locals; sbrect10 is_cfl_allowed narrowing BEFORE intra14's
INTRA_IN_INTER_MODE skip/mode split in read_intra_mode_rect), refusal_inventory.rs (kept
only intra14's narrowed "an intra-coded 16x4/4x16 strip ..."), decode_probe.rs (both).

## Fix applied this round
decode.rs `rect_inter_luma_set` + cdf_state.rs `TxbSet::LumaRect8x4Inter{,1}`: a SPLIT
16x4/4x16 inter strip resolves to an 8x4 leaf (`sub_tx_size_map[TX_16X4] == TX_8X4`,
oracle av1/common/common_data.h:180) and used to hit `unreachable!()`. Set = LumaRect4x8's
coefficient tables (Luma8 + 32-position eob_pt) with inter tx_type row TX_4X4
(`inter_tx_type_4` / `_set1`). `rect_inter_residual_supported` untouched — no refusal moved.
New diagnostic: `EC_SPLITSTRIP=1` prints mi_row/mi_col/bw/bh/depth at the counter bump.

## Gate: RED, but FIRING for the first time
- 8-bit arm: 40/40 refusals (histogram in the r2 report), never fires.
- 10-bit arm: seed 74 / cq 12 / cpu-used 2 decodes all 8 frames, fires depth1=1, and
  MISMATCHES ffmpeg: frame 1 Y first diff (163, 58), 5203 samples; frames 2-7 ~24.5k.
- Stream pinned: `<scratchpad>/intrasplit/s74.obu`, md5 50ea2b42423f1c8b4eed9fa48c4775a6
  (192x128 yuv420p10le mandelbrot start_scale=3.08; full aomenc recipe = the gate's).
- The ONLY split strip in the stream is decode-order frame 1, mi(8,0), 32x16, depth 1
  (pixels x0..31 y32..47) and ITS OWN footprint is pixel-clean: raster first diff is the
  LAST block of the same 32-px block row. So the divergence is downstream of the strip.
- NOT yet done: the msac RANGE ladder. Our tags: EC_MODE / EC_MODE_VAL / EC_STACK
  (EC_TRACE_MODE=1, 338 lines). aomdec: EC_IMODE* (key frame) + EC_MODE* / EC_MODE_MV /
  EC_STACK (219 lines) — emitters do not line up 1:1; ledger says our EC_MODE_VAL rng is
  printed at libaom's EC_MODE_MV point, so compare VALUES on the mode ladder and use
  EC_TRACE_COEFF ranges for the coefficient ladder.
- Do NOT use `decode_probe -o` for 10-bit triage: it writes 8-bit samples.

## Suite
385 passed / 3 failed / 35 ignored, 1082 s ($HOME/.cache/intrasplit-suite-r2.log).
2 = this lane's own arms. 1 = MERGE CROSS-PRODUCT, not this lane's code:
`a_real_aomenc_inter_sequence_with_16x16_level_1to4_partitions_decodes_pixel_exact`
(stream.rs:9167, frame 4 luma, attempt 1, 8-bit, arms [0,4,2,2]) — green on inter16ab's own
tip, red once r14 b86eb38 is in the same tree.

## Film probes (merged tree, EC_AV1_FINAL_DUMP=1, 0 frames dumped both)
- 2160p10 HDR segment -ss 1200: now stops at "a split intra strip whose transform unit is
  32x64" (was the non-skip rect-strip residual refusal at r1).
- 1080p10 segment -ss 1800: now stops at "a COMPOUND_WEDGE mask on a rectangular inter
  block" (was the intra 1:4 rect-strip refusal, lifted by lane-intra14).

## Exact next step (r3)
1. Ladder s74.obu frame 1 from the strip at mi(8,0) forward: ours
   `EC_TRACE_COEFF=1 EC_SPLITSTRIP=1 decode_probe s74.obu` vs
   `EC_TRACE_COEFF=1 ~/.cache/aom-oracle/build/aomdec --rawvideo -o /dev/null s74.obu`;
   find the first element whose RANGE step differs, in the blocks AFTER the strip in that
   block row (target block x160..191, y32..63).
2. Prime suspects, in order: the strip's TXFM_CONTEXT bands written by r1's `set_txfm_ctxs`
   tail (wrong span/value would desync the NEXT block's tx_depth), the depth-1 sub-unit
   walk in `decode_rect_split` for a 32x16 strip at 10 bit, and the skip/txfm side maps the
   strip leaves behind (class new-map-ignores-tile-edge / early-return-skips-tail).
3. Then re-run BOTH gate arms + the inter16ab 1:4 gate above (sibling-gate rule).
