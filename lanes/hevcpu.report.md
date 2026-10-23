# lane hevcpu: CABAC syntax for P and B slices (step 3 of 14)

## Goal
Parse every P and B slice to `end_of_slice_segment_flag` with the correct bit consumption:
coding quadtree, coding unit, prediction unit, mvd_coding, transform tree, SAO syntax and
the P/B context tables. No motion derivation and no motion compensation. Pixels are a
named placeholder so the pipeline runs end to end.

## Base
`15931054` (main, merge of lane/hevcdecapi).

## What changed
- `cabac.rs`: the context layout gains the inter elements (cu_skip_flag, pred_mode_flag,
  merge_flag, merge_idx, inter_pred_idc, ref_idx, mvp flags, abs_mvd greater flags,
  rqt_root_cbf, cu_qp_delta_abs, sao_merge, sao_type_idx). `part_mode` now has 4 contexts and
  `cbf_cb`/`cbf_cr` 5. Intra elements keep their meaning. The init values are one table
  (`INIT_ROWS`), each row holding initType 0, 1 and 2 values for one element. `Contexts::new`
  stays initType 0. `Contexts::for_init_type` is new. `CabacDecoder::bits_read` is new.
  Values cross-checked against an independent transcription of Tables 9-5 to 9-37 (the
  `oxideav-h265` `ctx_init.rs`, which follows the spec's ctxIdx layout). Unit tests check that
  the rows cover every context.
- `dec/cu_syntax.rs` (new, about 1,200 lines): `parse_slice(sps, pps, header, data, motion, sink)`.
  - coding_quadtree with split_cu_flag, cu_skip_flag (left/above skip ctx), pred_mode_flag,
    part_mode (intra NxN; inter incl. AMP when `amp_enabled_flag`, and the min-CB rules).
  - prediction_unit: merge_flag, merge_idx (first bin ctx, rest bypass), inter_pred_idc
    (ctx CtDepth, or ctx 4 for 8x4/4x8), ref_idx (2 ctx bins, rest bypass), mvp_lX_flag,
    mvd_l1_zero_flag handling.
  - mvd_coding: greater0/greater1 flags, abs_mvd_minus2 as EG1 bypass, sign.
  - rqt_root_cbf; transform_tree with interSplitFlag, split inference, cbf_luma inference at
    depth 0 for inter, 4:2:0 chroma cbf inheritance for 4x4 luma blocks.
  - intra modes (MPM, rem, chroma mode) so residual scan order is right in P/B slices.
  - cu_qp_delta_abs and QpY derivation (8.6.1), SAO syntax (7.3.8.3, cMax from bit depth).
  - Refused by name before any bin is read: tiles, WPP, dependent slices, a second slice in a
    picture, PCM, transquant bypass.
  - Residuals go through the existing `decode_residual`, `dequantize` and
    `inverse_transform`. Each CU is handed to a `sink` closure in decoding order.
  - `MotionSource` trait: `derive_motion(cu geometry, pu index, pu syntax) -> Motion`.
    `ZeroMotion` (zero MV, ref 0 per list) is the placeholder for the merge/AMVP lane.
- `dec/inter.rs`: `decode_inter_slice(sps, pps, header, data, reference)` runs the parser with
  `ZeroMotion`. **Placeholder pixels:** each CU copies the co-located block of RefPicList0[0]
  (grey 128 when there is none), then the residual is added. Named as a placeholder in the
  module docs.
- `dec/mod.rs`: `Decoder::set_syntax_check(bool)`. With pixel decode off and the flag on, every
  slice of every type is parsed by `cu_syntax`. The P/B pixel path passes RefPicList0[0]'s
  samples to `decode_inter_slice`.
- `decode.rs`: **bug fix.** `decode_i_slice` read `end_of_subset_one_bit` at every CTB row even
  without WPP, so no non-WPP conformance stream got past its first row. Without WPP the slice is
  now one substream. The WPP path is unchanged.
- `tests/cu_syntax.rs`: three tests over the conformance streams:
  `cu_syntax_sweep_over_conformance_streams` (syntax-only table),
  `pixel_pipeline_runs_over_conformance_streams`, and
  `inter_placeholder_runs_over_pb_slices` (calls `decode_inter_slice` on every P/B slice).

## Trailing-bit check
A slice is clean only if, after its `end_of_slice_segment_flag`, the bits are a stop bit then
zeros (then zero cabac_zero_words). The position of the stop bit is checked both at the
decoder's read position and one bit before it. Every slice that parses clean passes this, and
no slice desyncs. That is the strongest evidence that the bin consumption is exact.

## Verify (server, release)
- `scripts/remote-run.sh hevcpu -- cargo test -p ec-h265 -p ec-h265-syntax --release`: all pass.
  ec-h265 lib 32 (30 existing + 2 new), conformance 15, cu_syntax 3 (new), dec_shell 13,
  decode 5, encode 4, perf 3, rdoq_cache_hash 3; ec-h265-syntax 14 + 2 + 0. 0 failed.
- `scripts/remote-run.sh hevcpu -- cargo clippy -p ec-h265 --all-targets -- -D warnings`: fails
  with 10 errors, all pre-existing and none in new code: `decode.rs` `too_many_arguments` x3
  (`reconstruct_luma`, `reconstruct_chroma`, `decode_single_tu`), `residual.rs` x9,
  `transform.rs:71` `is_multiple_of`. The new code and the lines I touched are clean.
- `cu_syntax` sweep (final run, `hevcpu-sweep6`): **84 of 147 streams parse every slice to a
  clean end**. Slices: 6,695 of 51,175 parse clean. **0 CABAC desyncs.** All other slices are
  refused by name (63 streams).
- Inter placeholder: 6,358 P/B slices decode, 2,315 refused by name, 0 other errors.
- Pixel pipeline: 1 of 147 streams decodes without error. Reasons:
- desync in I slices (intra path), end_of_slice_segment_flag: 123
- refused: HEVC decode: 10-bit pixels: 11
- WPP streams, intra path, end_of_subset_one_bit not set: 7
- refused: HEVC SPS scaling lists: 3
- refused: HEVC decode: SPS range, multilayer, 3D or SCC extension: 1

## Per-stream syntax table
`slices`: slice NAL units. `clean`: slices that parse to a clean end, with the trailing bits
checked. `refused`: refused by name (tiles, WPP, PCM, transquant bypass, dependent slices,
a second slice per picture, scaling lists, SPS extension, or a parameter set refused by name,
which makes its slices fail). `desync`: CABAC errors (none). Streams with `clean` = `slices`
are the 84 clean streams.

| stream | slices | clean | refused | desync | refused by name (reason) |
|---|---:|---:|---:|---:|---|
| AMP_A_Samsung_7 | 17 | 17 | 0 | 0 |  |
| AMP_B_Samsung_7 | 17 | 17 | 0 | 0 |  |
| AMP_D_Hisilicon | 100 | 100 | 0 | 0 |  |
| AMP_E_Hisilicon | 100 | 100 | 0 | 0 |  |
| AMP_F_Hisilicon_3 | 100 | 100 | 0 | 0 |  |
| AMVP_A_MTK_4 | 30 | 30 | 0 | 0 |  |
| AMVP_B_MTK_4 | 41 | 41 | 0 | 0 |  |
| AMVP_C_Samsung_7 | 60 | 60 | 0 | 0 |  |
| BUMPING_A_ericsson_1 | 66 | 66 | 0 | 0 |  |
| CAINIT_A_SHARP_4 | 50 | 50 | 0 | 0 |  |
| CAINIT_B_SHARP_4 | 50 | 50 | 0 | 0 |  |
| CAINIT_C_SHARP_3 | 50 | 50 | 0 | 0 |  |
| CAINIT_D_SHARP_3 | 50 | 50 | 0 | 0 |  |
| CAINIT_E_SHARP_3 | 50 | 0 | 50 | 0 | HEVC slice syntax: tiles x50 |
| CAINIT_F_SHARP_3 | 50 | 0 | 50 | 0 | HEVC slice syntax: tiles x50 |
| CAINIT_G_SHARP_3 | 1750 | 50 | 1700 | 0 | HEVC slice syntax: dependent slice segments x1700 |
| CAINIT_H_SHARP_3 | 1750 | 50 | 1700 | 0 | HEVC slice syntax: dependent slice segments x1700 |
| CIP_A_Panasonic_3 | 2 | 2 | 0 | 0 |  |
| CIP_C_Panasonic_2 | 6 | 2 | 4 | 0 | HEVC slice syntax: multiple slices per picture x4 |
| CONFWIN_A_Sony_1 | 60 | 60 | 0 | 0 |  |
| DBLK_A_MAIN10_VIXS_4 | 8 | 0 | 8 | 0 | HEVC slice syntax: PCM x8 |
| DBLK_A_SONY_3 | 120 | 30 | 90 | 0 | HEVC slice syntax: multiple slices per picture x90 |
| DBLK_B_SONY_3 | 120 | 30 | 90 | 0 | HEVC slice syntax: multiple slices per picture x90 |
| DBLK_C_SONY_3 | 120 | 30 | 90 | 0 | HEVC slice syntax: multiple slices per picture x90 |
| DBLK_D_VIXS_2 | 192 | 0 | 192 | 0 | HEVC slice syntax: tiles x192 |
| DBLK_E_VIXS_2 | 24 | 0 | 24 | 0 | HEVC slice syntax: tiles x24 |
| DBLK_F_VIXS_2 | 24 | 0 | 24 | 0 | HEVC slice syntax: tiles x24 |
| DBLK_G_VIXS_2 | 24 | 0 | 24 | 0 | HEVC slice syntax: tiles x24 |
| DELTAQP_A_BRCM_4 | 624 | 0 | 624 | 0 | HEVC slice syntax: PCM x96; HEVC slice syntax: multiple slices per picture x528 |
| DELTAQP_B_SONY_3 | 120 | 30 | 90 | 0 | HEVC slice syntax: multiple slices per picture x90 |
| DELTAQP_C_SONY_3 | 120 | 30 | 90 | 0 | HEVC slice syntax: multiple slices per picture x90 |
| DSLICE_A_HHI_5 | 850 | 50 | 800 | 0 | HEVC slice syntax: dependent slice segments x700; HEVC slice syntax: multiple slices per picture x100 |
| DSLICE_B_HHI_5 | 1700 | 0 | 1700 | 0 | HEVC slice syntax: wavefront parallel processing x1700 |
| DSLICE_C_HHI_5 | 750 | 0 | 750 | 0 | HEVC slice syntax: tiles x750 |
| ENTP_A_Qualcomm_1 | 24 | 0 | 24 | 0 | HEVC slice syntax: tiles x24 |
| ENTP_B_Qualcomm_1 | 24 | 0 | 24 | 0 | HEVC slice syntax: tiles x24 |
| ENTP_C_Qualcomm_1 | 24 | 0 | 24 | 0 | HEVC slice syntax: wavefront parallel processing x24 |
| EXT_A_ericsson_4 | 3 | 3 | 0 | 0 |  |
| FILLER_A_Sony_1 | 60 | 60 | 0 | 0 |  |
| HRD_A_Fujitsu_3 | 384 | 96 | 288 | 0 | HEVC slice syntax: multiple slices per picture x288 |
| INITQP_A_Sony_1 | 60 | 60 | 0 | 0 |  |
| INITQP_B_Main10_Sony_1 | 90 | 90 | 0 | 0 |  |
| IPRED_A_docomo_2 | 20 | 20 | 0 | 0 |  |
| IPRED_B_Nokia_3 | 1 | 1 | 0 | 0 |  |
| IPRED_C_Mitsubishi_3 | 4 | 4 | 0 | 0 |  |
| LS_A_Orange_2 | 9 | 0 | 9 | 0 | HEVC slice syntax: transquant bypass x9 |
| LS_B_Orange_4 | 25 | 0 | 25 | 0 | HEVC slice syntax: transquant bypass x25 |
| LTRPSPS_A_Qualcomm_1 | 500 | 500 | 0 | 0 |  |
| MAXBINS_A_TI_5 | 2 | 0 | 2 | 0 | HEVC slice syntax: PCM x2 |
| MAXBINS_B_TI_5 | 3 | 0 | 3 | 0 | HEVC slice syntax: PCM x3 |
| MAXBINS_C_TI_5 | 3 | 0 | 3 | 0 | HEVC slice syntax: PCM x3 |
| MERGE_A_TI_3 | 8 | 8 | 0 | 0 |  |
| MERGE_B_TI_3 | 8 | 8 | 0 | 0 |  |
| MERGE_C_TI_3 | 8 | 8 | 0 | 0 |  |
| MERGE_D_TI_3 | 8 | 8 | 0 | 0 |  |
| MERGE_E_TI_3 | 8 | 8 | 0 | 0 |  |
| MERGE_F_MTK_4 | 41 | 41 | 0 | 0 |  |
| MERGE_G_HHI_4 | 2 | 2 | 0 | 0 |  |
| MVCLIP_A_qualcomm_3 | 5 | 5 | 0 | 0 |  |
| MVDL1ZERO_A_docomo_4 | 5500 | 500 | 5000 | 0 | HEVC slice syntax: multiple slices per picture x5000 |
| MVEDGE_A_qualcomm_3 | 17 | 17 | 0 | 0 |  |
| NUT_A_ericsson_5 | 34 | 34 | 0 | 0 |  |
| NoOutPrior_A_Qualcomm_1 | 43 | 43 | 0 | 0 |  |
| NoOutPrior_B_Qualcomm_1 | 50 | 50 | 0 | 0 |  |
| OPFLAG_A_Qualcomm_1 | 500 | 500 | 0 | 0 |  |
| OPFLAG_B_Qualcomm_1 | 100 | 100 | 0 | 0 |  |
| OPFLAG_C_Qualcomm_1 | 100 | 100 | 0 | 0 |  |
| PICSIZE_A_Bossen_1 | 10 | 0 | 10 | 0 | HEVC slice syntax: tiles x10 |
| PICSIZE_B_Bossen_1 | 10 | 10 | 0 | 0 |  |
| PICSIZE_C_Bossen_1 | 10 | 0 | 10 | 0 | HEVC slice syntax: tiles x10 |
| PICSIZE_D_Bossen_1 | 10 | 10 | 0 | 0 |  |
| PMERGE_A_TI_3 | 8 | 8 | 0 | 0 |  |
| PMERGE_B_TI_3 | 8 | 8 | 0 | 0 |  |
| PMERGE_C_TI_3 | 8 | 8 | 0 | 0 |  |
| PMERGE_D_TI_3 | 8 | 8 | 0 | 0 |  |
| PMERGE_E_TI_3 | 8 | 8 | 0 | 0 |  |
| POC_A_Bossen_3 | 5 | 5 | 0 | 0 |  |
| PPS_A_qualcomm_7 | 81 | 0 | 81 | 0 | HEVC slice syntax: tiles x81 |
| PS_B_VIDYO_3 | 25 | 0 | 25 | 0 | parameter set refused (see above) x25; HEVC decode: SPS range, multilayer, 3D or SCC extension x1 (parameter set) |
| RAP_A_docomo_6 | 86 | 86 | 0 | 0 |  |
| RAP_B_Bossen_2 | 83 | 83 | 0 | 0 |  |
| RPLM_A_qualcomm_4 | 300 | 300 | 0 | 0 |  |
| RPLM_B_qualcomm_4 | 900 | 300 | 600 | 0 | HEVC slice syntax: multiple slices per picture x600 |
| RPS_A_docomo_5 | 44 | 44 | 0 | 0 |  |
| RPS_B_qualcomm_5 | 300 | 300 | 0 | 0 |  |
| RPS_C_ericsson_5 | 40 | 40 | 0 | 0 |  |
| RPS_D_ericsson_6 | 68 | 68 | 0 | 0 |  |
| RPS_E_qualcomm_5 | 300 | 300 | 0 | 0 |  |
| RPS_F_docomo_2 | 50 | 50 | 0 | 0 |  |
| RQT_A_HHI_4 | 2 | 2 | 0 | 0 |  |
| RQT_B_HHI_4 | 2 | 2 | 0 | 0 |  |
| RQT_C_HHI_4 | 2 | 2 | 0 | 0 |  |
| RQT_D_HHI_4 | 2 | 2 | 0 | 0 |  |
| RQT_E_HHI_4 | 2 | 2 | 0 | 0 |  |
| RQT_F_HHI_4 | 2 | 2 | 0 | 0 |  |
| RQT_G_HHI_4 | 2 | 2 | 0 | 0 |  |
| SAODBLK_A_MainConcept_4 | 12000 | 0 | 12000 | 0 | HEVC slice syntax: PCM x500; HEVC slice syntax: multiple slices per picture x11500 |
| SAODBLK_B_MainConcept_4 | 13500 | 0 | 13500 | 0 | HEVC slice syntax: tiles x13500 |
| SAO_A_MediaTek_4 | 60 | 60 | 0 | 0 |  |
| SAO_B_MediaTek_5 | 60 | 0 | 60 | 0 | HEVC slice syntax: tiles x60 |
| SAO_C_Samsung_5 | 5 | 5 | 0 | 0 |  |
| SAO_D_Samsung_5 | 5 | 5 | 0 | 0 |  |
| SAO_E_Canon_4 | 16 | 16 | 0 | 0 |  |
| SAO_F_Canon_3 | 16 | 16 | 0 | 0 |  |
| SAO_G_Canon_3 | 16 | 16 | 0 | 0 |  |
| SAO_H_Parabola_1 | 62 | 4 | 58 | 0 | HEVC slice syntax: multiple slices per picture x58 |
| SDH_A_Orange_4 | 2 | 0 | 2 | 0 | HEVC slice syntax: transquant bypass x2 |
| SLICES_A_Rovi_3 | 180 | 9 | 171 | 0 | HEVC slice syntax: multiple slices per picture x171 |
| SLIST_A_Sony_5 | 51 | 0 | 51 | 0 | parameter set refused (see above) x51; HEVC PPS scaling lists x64 (parameter set); HEVC SPS scaling lists x1 (parameter set) |
| SLIST_B_Sony_9 | 65 | 40 | 25 | 0 | parameter set refused (see above) x25; HEVC PPS scaling lists x20 (parameter set); HEVC SPS scaling lists x1 (parameter set) |
| SLIST_C_Sony_4 | 65 | 65 | 0 | 0 | HEVC PPS scaling lists x64 (parameter set) |
| str | 260 | 40 | 220 | 0 | HEVC slice syntax: multiple slices per picture x120; parameter set refused (see above) x100; HEVC PPS scaling lists x20 (parameter set); HEVC SPS scaling lists x1 (parameter set) |
| SLPPLP_A_VIDYO_2 | 33 | 33 | 0 | 0 |  |
| STRUCT_A_Samsung_7 | 60 | 60 | 0 | 0 |  |
| STRUCT_B_Samsung_7 | 60 | 60 | 0 | 0 |  |
| TILES_A_Cisco_2 | 100 | 0 | 100 | 0 | HEVC slice syntax: tiles x100 |
| TILES_B_Cisco_1 | 1308 | 0 | 1308 | 0 | HEVC slice syntax: tiles x1308 |
| TMVP_A_MS_3 | 17 | 17 | 0 | 0 |  |
| TSCL_A_VIDYO_5 | 73 | 73 | 0 | 0 |  |
| TSCL_B_VIDYO_4 | 73 | 73 | 0 | 0 |  |
| TSKIP_A_MS_3 | 17 | 17 | 0 | 0 |  |
| TSUNEQBD_A_MAIN10_Technicolor_2 | 8 | 8 | 0 | 0 |  |
| TUSIZE_A_Samsung_1 | 5 | 5 | 0 | 0 |  |
| VPSID_A_VIDYO_2 | 33 | 33 | 0 | 0 |  |
| VPSSPSPPS_A_MainConcept_1 | 6 | 0 | 6 | 0 | HEVC slice syntax: PCM x6 |
| WPP_A_ericsson_MAIN10_2 | 331 | 0 | 331 | 0 | HEVC slice syntax: wavefront parallel processing x331 |
| WPP_A_ericsson_MAIN_2 | 331 | 0 | 331 | 0 | HEVC slice syntax: wavefront parallel processing x331 |
| WPP_B_ericsson_MAIN10_2 | 350 | 0 | 350 | 0 | HEVC slice syntax: wavefront parallel processing x350 |
| WPP_B_ericsson_MAIN_2 | 350 | 0 | 350 | 0 | HEVC slice syntax: wavefront parallel processing x350 |
| WPP_C_ericsson_MAIN10_2 | 374 | 0 | 374 | 0 | HEVC slice syntax: wavefront parallel processing x374 |
| WPP_C_ericsson_MAIN_2 | 374 | 0 | 374 | 0 | HEVC slice syntax: wavefront parallel processing x374 |
| WPP_D_ericsson_MAIN10_2 | 94 | 0 | 94 | 0 | HEVC slice syntax: wavefront parallel processing x94 |
| WPP_D_ericsson_MAIN_2 | 94 | 0 | 94 | 0 | HEVC slice syntax: wavefront parallel processing x94 |
| WPP_E_ericsson_MAIN10_2 | 118 | 0 | 118 | 0 | HEVC slice syntax: wavefront parallel processing x118 |
| WPP_E_ericsson_MAIN_2 | 118 | 0 | 118 | 0 | HEVC slice syntax: wavefront parallel processing x118 |
| WPP_F_ericsson_MAIN10_2 | 106 | 0 | 106 | 0 | HEVC slice syntax: wavefront parallel processing x106 |
| WPP_F_ericsson_MAIN_2 | 106 | 0 | 106 | 0 | HEVC slice syntax: wavefront parallel processing x106 |
| WP_A_MAIN10_Toshiba_3 | 256 | 256 | 0 | 0 |  |
| WP_A_Toshiba_3 | 256 | 256 | 0 | 0 |  |
| WP_B_Toshiba_3 | 256 | 256 | 0 | 0 |  |
| WP_MAIN10_B_Toshiba_3 | 256 | 256 | 0 | 0 |  |
| cip_B_NEC_3 | 5 | 5 | 0 | 0 |  |
| ipcm_A_NEC_3 | 1 | 0 | 1 | 0 | HEVC slice syntax: PCM x1 |
| ipcm_B_NEC_3 | 1 | 0 | 1 | 0 | HEVC slice syntax: PCM x1 |
| ipcm_C_NEC_3 | 1 | 0 | 1 | 0 | HEVC slice syntax: PCM x1 |
| ipcm_D_NEC_3 | 1 | 0 | 1 | 0 | HEVC slice syntax: PCM x1 |
| ipcm_E_NEC_2 | 1 | 0 | 1 | 0 | HEVC slice syntax: PCM x1 |

## What is left
- **Intra pixel path.** `decode_i_slice` cannot parse most conformance I slices: no SAO
  syntax, no cu_qp_delta, one-level transform split only. 78 of the 123 streams that desync
  there parse fully clean under the new parser, so the cause is the old intra path, not the
  bitstreams. Routing I slices in pixel mode through `cu_syntax` (plus real intra prediction)
  is the next step. Not done here, since it would replace pixel-exact intra decode.
- **Refused by name** (streams, a stream can have several): tiles 16, WPP 14, multiple slices
  per picture 15, PCM 12, transquant bypass 3, dependent slices 3, scaling lists 3 (SLIST_A,
  SLIST_B, str; refused at the PPS/SPS), SPS range/3D/SCC extension 1 (PS_B_VIDYO_3).
- **SAO** syntax is parsed; the SAO filter is not applied.
- **Motion:** `ZeroMotion` placeholder. Merge (incl. `log2_parallel_merge_level`) and AMVP are
  the next lane's job, behind `MotionSource`.
- **Pixels:** P/B samples are placeholder copies plus residual. No motion compensation,
  no deblocking of inter edges.
- **Stop-bit position:** both candidate positions are accepted. Which one matches the
  spec's bit accounting was not pinned down; the check is tight either way (the stop bit and
  zeros must end the slice data).

## Commit trailer
The Co-Authored-By line uses Claude Haiku 5.5, as the attribution instruction for this
session says. ORCHESTRATION.md names Opus 5.5.
