# lane-lossless — AV1 lossless (qindex 0) decode

Branch `lane-lossless`, base `7ba28bcd`. Status: IN PROGRESS.

## Done (commits b56bb30f, d26c55bb wip)
- `transform.rs::inverse_wht4x4` — libaom `av1_iwht4x4_16_add_c`
  (`aom_dsp/inv_txfm.c:22`), inputs shifted by `UNIT_QUANT_SHIFT` (2). Test
  `transform::lossless_tx_tests::the_walsh_hadamard_round_trip_is_exact` PASSES.
- `TxParams::lossless` + `dequant_and_inverse_wht4x4`: the residual of a
  lossless unit comes off the WHT, not the DCT/ADST network
  (`av1/common/idct.c` `inv_txfm_add`: `if (txfm_param->lossless) { assert(tx_type
  == DCT_DCT); av1_iwht4x4_add(...); return; }`).
- Per-frame `CodedLossless` flag (`decode::set_lossless`/`lossless`), set in
  `stream.rs` where the refusal used to be. A frame MIXING lossless and lossy
  segments is still refused by name (all rules are per segment there).
- TX_4X4 forcing: `decode_block` (tables/scans/tx swapped to the 4x4 ones),
  `decode_leaf8`, and `depth_to_tx_wh` (every HORZ/VERT strip reader) --
  libaom `read_tx_size` (`decodeframe.c`): `if (xd->lossless[segment_id])
  return TX_4X4;` before anything else.
- No `tx_type` symbol, DCT_DCT on every plane: libaom `read_tx_type`
  (`decodetxb.c`) returns early at `qindex == 0`; `av1_get_tx_type`
  (`blockd.h`) returns DCT_DCT for a lossless block.
- `is_cfl_allowed` narrowing (`blockd.h`): under lossless CfL is offered only
  when the chroma plane block is BLOCK_4X4, i.e. a luma block <= 8x8
  (`decode::cfl_allowed_px`, the one point every intra reader routes through).
- `txb_skip_chroma_4` widened 3 -> 6 rows: `get_txb_ctx`'s `+10` rows
  (chroma plane block larger than its transform) are reachable at TX_4X4 only
  on a lossless frame -- `av1_default_txb_skip_cdfs[0][TX_4X4][10..13]` =
  9961/30242/32117 and are the neutral 16384 in every other q-context, which
  is libaom's own confirmation of the rule.

## Remaining
- The 64x64 synthetic key frame (`ffmpeg -c:v libaom-av1 -crf 0 -b:v 0`) now
  parses the whole tile but still differs from ffmpeg at luma sample 0:
  desync is inside the first block's coefficients (mode syntax of block 1
  matches aomdec's `EC_ISTEP`, block 2's range does not).
- Inter-frame readers (`decode_inter_block*`, sub8) not yet forced to TX_4X4.
- Filter-stage skips (deblock/CDEF/LR) are header-driven and believed already
  handled by the header parser; unverified.
- Fixtures + gates: none added yet; the retired refusal gate
  `a_lossless_libaom_stream_is_refused_by_name` still asserts the old refusal
  and will FAIL until it is swapped for the decode-exact gate.
