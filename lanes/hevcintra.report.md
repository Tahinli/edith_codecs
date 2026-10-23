# lane hevcintra: pixel-exact intra decode through cu_syntax

## Goal

Make ec-h265 produce pixel-exact intra output on the JCT-VC conformance streams through the new parser `dec/cu_syntax.rs`: intra reconstruction, QP derivation (8.6.1), deblocking (8.7.2), SAO (8.7.3), PCM and transquant bypass, checked against the decoded picture hash SEI on every intra picture.

Base: `1b962d32` (main at lane creation). Main was merged in at `84a1a2d6` (scaling lists) and the lane is on top of `fc963179`.

## Changes

- `src/intra.rs`: generic over the sample type (u8 and u16), bit-depth aware substitution, filtering (strong smoothing threshold `1 << (bd - 5)`) and prediction.
- `src/transform.rs`: bit-depth variants of dequantization, inverse transform, transform skip and chroma QP (Table 8-10); one weighted core shared with main's `dequantize_with`.
- `src/cabac.rs`: `CU_TRANSQUANT_BYPASS` context, `data()`, `restart_at_byte()`.
- `src/decode.rs`: `DecodedPicture` now holds `u16` planes with `bit_depth`; `decode_idr_au` is a wrapper over `dec::Decoder`; the old `decode_i_slice` is removed.
- `src/dec/pic.rs` (new): per-picture block info, QP, edges, SAO parameters, slice filter flags, z-scan availability (6.4.1).
- `src/dec/recon.rs` (new): intra reference gathering with constrained intra prediction, residual add, plane views.
- `src/dec/cu_syntax.rs`: every slice pixel-decodes through `parse_slice_into`; PCM, transquant bypass, QP prediction and cu_qp_delta with QpBdOffset, scaling factors (8.6.4.2: flat 16 when lists are off or for transform-skipped blocks over 4x4), SAO parse, and WPP (contexts synced from the second CTB of the row above, arithmetic decoder restarted per substream).
- `src/dec/deblock.rs` (new): deblocking 8.7.2 on the 8x8 grid, bS from intra/cbf, slice and tile boundary flags, pcm and bypass exclusions.
- `src/dec/sao.rs` (new): SAO 8.7.3, band and edge, applied after deblocking.
- `src/dec/inter.rs`: reduced to a placeholder (copy of the reference or grey, plus residual); no motion compensation yet.
- `src/dec/mod.rs`, `src/dec/meta.rs`, `src/dec/rps.rs`: picture finish runs deblock then SAO; `PictureMeta.intra_only`; RPS grey fill takes a bit depth.
- `tests/conformance_intra.rs` (new): decodes each stream, compares every intra picture with its MD5 SEI, prints the per-stream table (`--nocapture`).
- `tests/dec_meta.rs`, `tests/dec_shell.rs`, `tests/cu_syntax.rs`, `tests/decode.rs`, `tests/scaling_lists.rs`: updated to the `u16` planes and the new entry points. `cu_syntax.rs` now calls `parse_slice` in place of the removed `decode_inter_slice`.
- Encoder (`src/ctu.rs`): intra calls pass bit depth 8; `src/deblock.rs` (encoder filter) unchanged.

## Verify

| Command | Result |
|---|---|
| `scripts/remote-run.sh hevcintra -- cargo clippy -p ec-h265 -p ec-h265-syntax --all-targets -- -D warnings` | clean (exit 0) |
| `scripts/remote-run.sh hevcintra -- cargo test -p ec-h265 -p ec-h265-syntax --release` | all 16 test binaries pass, 0 failed (includes encoder round trips, WPP, scaling lists, deblock and SAO unit tests, dec_shell RPS tests) |
| `scripts/remote-run.sh hevcintra -- cargo check --workspace --all-targets` | exit 0; warnings only in ec-av1 (not this lane) |
| `scripts/remote-run.sh hevcintra -- cargo test -p ec-h265 --release --test conformance_intra -- --nocapture` | total: 269 intra pictures checked, 269 exact, 100/147 streams accepted |

## Intra exactness

Accepted streams: 100 of 147. Intra pictures checked against the SEI MD5: 269, exact: 269. No mismatches.

Refused streams by reason:

| Reason | Streams |
|---|---|
| HEVC decode: multiple slices per picture | 30 |
| HEVC slice syntax: tiles | 16 |
| HEVC decode: unequal luma and chroma bit depths | 1 |

"no intra pictures with an MD5 hash" means the stream was accepted but has no intra picture carrying an MD5 SEI (no hash SEI, or inter-only), so nothing was checked for it.

## Per-stream table

| Stream | Accepted | Intra pictures checked | Exact | Note |
|---|---|---|---|---|
| AMP_A_Samsung_7 | yes | 0 | 0 | no intra pictures with an MD5 hash |
| AMP_B_Samsung_7 | yes | 0 | 0 | no intra pictures with an MD5 hash |
| AMP_D_Hisilicon | yes | 0 | 0 | no intra pictures with an MD5 hash |
| AMP_E_Hisilicon | yes | 0 | 0 | no intra pictures with an MD5 hash |
| AMP_F_Hisilicon_3 | yes | 0 | 0 | no intra pictures with an MD5 hash |
| AMVP_A_MTK_4 | yes | 0 | 0 | no intra pictures with an MD5 hash |
| AMVP_B_MTK_4 | yes | 0 | 0 | no intra pictures with an MD5 hash |
| AMVP_C_Samsung_7 | yes | 0 | 0 | no intra pictures with an MD5 hash |
| BUMPING_A_ericsson_1 | yes | 7 | 7 | ok |
| CAINIT_A_SHARP_4 | yes | 2 | 2 | ok |
| CAINIT_B_SHARP_4 | yes | 2 | 2 | ok |
| CAINIT_C_SHARP_3 | yes | 1 | 1 | ok |
| CAINIT_D_SHARP_3 | yes | 1 | 1 | ok |
| CAINIT_E_SHARP_3 | no | 0 | 0 | refused: HEVC slice syntax: tiles (tile scan order is not implemented) |
| CAINIT_F_SHARP_3 | no | 0 | 0 | refused: HEVC slice syntax: tiles (tile scan order is not implemented) |
| CAINIT_G_SHARP_3 | no | 0 | 0 | refused: HEVC decode: multiple slices per picture (pixel decode covers one slice segment per picture) |
| CAINIT_H_SHARP_3 | no | 0 | 0 | refused: HEVC decode: multiple slices per picture (pixel decode covers one slice segment per picture) |
| CIP_A_Panasonic_3 | yes | 1 | 1 | ok |
| CIP_C_Panasonic_2 | no | 0 | 0 | refused: HEVC decode: multiple slices per picture (pixel decode covers one slice segment per picture) |
| CONFWIN_A_Sony_1 | yes | 0 | 0 | no intra pictures with an MD5 hash |
| DBLK_A_MAIN10_VIXS_4 | yes | 0 | 0 | no intra pictures with an MD5 hash |
| DBLK_A_SONY_3 | no | 0 | 0 | refused: HEVC decode: multiple slices per picture (pixel decode covers one slice segment per picture) |
| DBLK_B_SONY_3 | no | 0 | 0 | refused: HEVC decode: multiple slices per picture (pixel decode covers one slice segment per picture) |
| DBLK_C_SONY_3 | no | 0 | 0 | refused: HEVC decode: multiple slices per picture (pixel decode covers one slice segment per picture) |
| DBLK_D_VIXS_2 | no | 0 | 0 | refused: HEVC slice syntax: tiles (tile scan order is not implemented) |
| DBLK_E_VIXS_2 | no | 0 | 0 | refused: HEVC slice syntax: tiles (tile scan order is not implemented) |
| DBLK_F_VIXS_2 | no | 0 | 0 | refused: HEVC slice syntax: tiles (tile scan order is not implemented) |
| DBLK_G_VIXS_2 | no | 0 | 0 | refused: HEVC slice syntax: tiles (tile scan order is not implemented) |
| DELTAQP_A_BRCM_4 | no | 0 | 0 | refused: HEVC decode: multiple slices per picture (pixel decode covers one slice segment per picture) |
| DELTAQP_B_SONY_3 | no | 0 | 0 | refused: HEVC decode: multiple slices per picture (pixel decode covers one slice segment per picture) |
| DELTAQP_C_SONY_3 | no | 0 | 0 | refused: HEVC decode: multiple slices per picture (pixel decode covers one slice segment per picture) |
| DSLICE_A_HHI_5 | no | 0 | 0 | refused: HEVC decode: multiple slices per picture (pixel decode covers one slice segment per picture) |
| DSLICE_B_HHI_5 | no | 0 | 0 | refused: HEVC decode: multiple slices per picture (pixel decode covers one slice segment per picture) |
| DSLICE_C_HHI_5 | no | 0 | 0 | refused: HEVC slice syntax: tiles (tile scan order is not implemented) |
| ENTP_A_Qualcomm_1 | no | 0 | 0 | refused: HEVC slice syntax: tiles (tile scan order is not implemented) |
| ENTP_B_Qualcomm_1 | no | 0 | 0 | refused: HEVC slice syntax: tiles (tile scan order is not implemented) |
| ENTP_C_Qualcomm_1 | yes | 1 | 1 | ok |
| EXT_A_ericsson_4 | yes | 1 | 1 | ok |
| FILLER_A_Sony_1 | yes | 0 | 0 | no intra pictures with an MD5 hash |
| HRD_A_Fujitsu_3 | no | 0 | 0 | refused: HEVC decode: multiple slices per picture (pixel decode covers one slice segment per picture) |
| INITQP_A_Sony_1 | yes | 0 | 0 | no intra pictures with an MD5 hash |
| INITQP_B_Main10_Sony_1 | yes | 0 | 0 | no intra pictures with an MD5 hash |
| IPRED_A_docomo_2 | yes | 20 | 20 | ok |
| IPRED_B_Nokia_3 | yes | 1 | 1 | ok |
| IPRED_C_Mitsubishi_3 | yes | 4 | 4 | ok |
| LS_A_Orange_2 | yes | 1 | 1 | ok |
| LS_B_Orange_4 | yes | 1 | 1 | ok |
| LTRPSPS_A_Qualcomm_1 | yes | 0 | 0 | no intra pictures with an MD5 hash |
| MAXBINS_A_TI_5 | yes | 0 | 0 | no intra pictures with an MD5 hash |
| MAXBINS_B_TI_5 | yes | 0 | 0 | no intra pictures with an MD5 hash |
| MAXBINS_C_TI_5 | yes | 0 | 0 | no intra pictures with an MD5 hash |
| MERGE_A_TI_3 | yes | 1 | 1 | ok |
| MERGE_B_TI_3 | yes | 1 | 1 | ok |
| MERGE_C_TI_3 | yes | 1 | 1 | ok |
| MERGE_D_TI_3 | yes | 1 | 1 | ok |
| MERGE_E_TI_3 | yes | 1 | 1 | ok |
| MERGE_F_MTK_4 | yes | 0 | 0 | no intra pictures with an MD5 hash |
| MERGE_G_HHI_4 | yes | 1 | 1 | ok |
| MVCLIP_A_qualcomm_3 | yes | 1 | 1 | ok |
| MVDL1ZERO_A_docomo_4 | no | 0 | 0 | refused: HEVC decode: multiple slices per picture (pixel decode covers one slice segment per picture) |
| MVEDGE_A_qualcomm_3 | yes | 1 | 1 | ok |
| NUT_A_ericsson_5 | yes | 7 | 7 | ok |
| NoOutPrior_A_Qualcomm_1 | yes | 2 | 2 | ok |
| NoOutPrior_B_Qualcomm_1 | yes | 2 | 2 | ok |
| OPFLAG_A_Qualcomm_1 | yes | 16 | 16 | ok |
| OPFLAG_B_Qualcomm_1 | yes | 98 | 98 | ok |
| OPFLAG_C_Qualcomm_1 | yes | 4 | 4 | ok |
| PICSIZE_A_Bossen_1 | no | 0 | 0 | refused: HEVC slice syntax: tiles (tile scan order is not implemented) |
| PICSIZE_B_Bossen_1 | yes | 0 | 0 | no intra pictures with an MD5 hash |
| PICSIZE_C_Bossen_1 | no | 0 | 0 | refused: HEVC slice syntax: tiles (tile scan order is not implemented) |
| PICSIZE_D_Bossen_1 | yes | 0 | 0 | no intra pictures with an MD5 hash |
| PMERGE_A_TI_3 | yes | 1 | 1 | ok |
| PMERGE_B_TI_3 | yes | 1 | 1 | ok |
| PMERGE_C_TI_3 | yes | 1 | 1 | ok |
| PMERGE_D_TI_3 | yes | 1 | 1 | ok |
| PMERGE_E_TI_3 | yes | 1 | 1 | ok |
| POC_A_Bossen_3 | yes | 0 | 0 | no intra pictures with an MD5 hash |
| PPS_A_qualcomm_7 | no | 0 | 0 | refused: HEVC slice syntax: tiles (tile scan order is not implemented) |
| PS_B_VIDYO_3 | yes | 1 | 1 | ok |
| RAP_A_docomo_6 | yes | 0 | 0 | no intra pictures with an MD5 hash |
| RAP_B_Bossen_2 | yes | 4 | 4 | ok |
| RPLM_A_qualcomm_4 | yes | 10 | 10 | ok |
| RPLM_B_qualcomm_4 | no | 0 | 0 | refused: HEVC decode: multiple slices per picture (pixel decode covers one slice segment per picture) |
| RPS_A_docomo_5 | yes | 0 | 0 | no intra pictures with an MD5 hash |
| RPS_B_qualcomm_5 | yes | 10 | 10 | ok |
| RPS_C_ericsson_5 | yes | 1 | 1 | ok |
| RPS_D_ericsson_6 | yes | 5 | 5 | ok |
| RPS_E_qualcomm_5 | yes | 10 | 10 | ok |
| RPS_F_docomo_2 | yes | 0 | 0 | no intra pictures with an MD5 hash |
| RQT_A_HHI_4 | yes | 1 | 1 | ok |
| RQT_B_HHI_4 | yes | 1 | 1 | ok |
| RQT_C_HHI_4 | yes | 1 | 1 | ok |
| RQT_D_HHI_4 | yes | 1 | 1 | ok |
| RQT_E_HHI_4 | yes | 1 | 1 | ok |
| RQT_F_HHI_4 | yes | 1 | 1 | ok |
| RQT_G_HHI_4 | yes | 1 | 1 | ok |
| SAODBLK_A_MainConcept_4 | no | 0 | 0 | refused: HEVC decode: multiple slices per picture (pixel decode covers one slice segment per picture) |
| SAODBLK_B_MainConcept_4 | no | 0 | 0 | refused: HEVC slice syntax: tiles (tile scan order is not implemented) |
| SAO_A_MediaTek_4 | yes | 2 | 2 | ok |
| SAO_B_MediaTek_5 | no | 0 | 0 | refused: HEVC slice syntax: tiles (tile scan order is not implemented) |
| SAO_C_Samsung_5 | yes | 0 | 0 | no intra pictures with an MD5 hash |
| SAO_D_Samsung_5 | yes | 0 | 0 | no intra pictures with an MD5 hash |
| SAO_E_Canon_4 | yes | 0 | 0 | no intra pictures with an MD5 hash |
| SAO_F_Canon_3 | yes | 0 | 0 | no intra pictures with an MD5 hash |
| SAO_G_Canon_3 | yes | 0 | 0 | no intra pictures with an MD5 hash |
| SAO_H_Parabola_1 | no | 0 | 0 | refused: HEVC decode: multiple slices per picture (pixel decode covers one slice segment per picture) |
| SDH_A_Orange_4 | yes | 1 | 1 | ok |
| SLICES_A_Rovi_3 | no | 0 | 0 | refused: HEVC decode: multiple slices per picture (pixel decode covers one slice segment per picture) |
| SLIST_A_Sony_5 | yes | 3 | 3 | ok |
| SLIST_B_Sony_9 | yes | 3 | 3 | ok |
| SLIST_C_Sony_4 | yes | 3 | 3 | ok |
| str | no | 0 | 0 | refused: HEVC decode: multiple slices per picture (pixel decode covers one slice segment per picture) |
| SLPPLP_A_VIDYO_2 | yes | 1 | 1 | ok |
| STRUCT_A_Samsung_7 | yes | 0 | 0 | no intra pictures with an MD5 hash |
| STRUCT_B_Samsung_7 | yes | 0 | 0 | no intra pictures with an MD5 hash |
| TILES_A_Cisco_2 | no | 0 | 0 | refused: HEVC slice syntax: tiles (tile scan order is not implemented) |
| TILES_B_Cisco_1 | no | 0 | 0 | refused: HEVC slice syntax: tiles (tile scan order is not implemented) |
| TMVP_A_MS_3 | yes | 1 | 1 | ok |
| TSCL_A_VIDYO_5 | yes | 3 | 3 | ok |
| TSCL_B_VIDYO_4 | yes | 1 | 1 | ok |
| TSKIP_A_MS_3 | yes | 1 | 1 | ok |
| TSUNEQBD_A_MAIN10_Technicolor_2 | no | 0 | 0 | refused: HEVC decode: unequal luma and chroma bit depths (one bit depth for every plane is implemented) |
| TUSIZE_A_Samsung_1 | yes | 0 | 0 | no intra pictures with an MD5 hash |
| VPSID_A_VIDYO_2 | yes | 1 | 1 | ok |
| VPSSPSPPS_A_MainConcept_1 | yes | 0 | 0 | no intra pictures with an MD5 hash |
| WPP_A_ericsson_MAIN10_2 | no | 0 | 0 | refused: HEVC decode: multiple slices per picture (pixel decode covers one slice segment per picture) |
| WPP_A_ericsson_MAIN_2 | no | 0 | 0 | refused: HEVC decode: multiple slices per picture (pixel decode covers one slice segment per picture) |
| WPP_B_ericsson_MAIN10_2 | no | 0 | 0 | refused: HEVC decode: multiple slices per picture (pixel decode covers one slice segment per picture) |
| WPP_B_ericsson_MAIN_2 | no | 0 | 0 | refused: HEVC decode: multiple slices per picture (pixel decode covers one slice segment per picture) |
| WPP_C_ericsson_MAIN10_2 | no | 0 | 0 | refused: HEVC decode: multiple slices per picture (pixel decode covers one slice segment per picture) |
| WPP_C_ericsson_MAIN_2 | no | 0 | 0 | refused: HEVC decode: multiple slices per picture (pixel decode covers one slice segment per picture) |
| WPP_D_ericsson_MAIN10_2 | no | 0 | 0 | refused: HEVC decode: multiple slices per picture (pixel decode covers one slice segment per picture) |
| WPP_D_ericsson_MAIN_2 | no | 0 | 0 | refused: HEVC decode: multiple slices per picture (pixel decode covers one slice segment per picture) |
| WPP_E_ericsson_MAIN10_2 | no | 0 | 0 | refused: HEVC decode: multiple slices per picture (pixel decode covers one slice segment per picture) |
| WPP_E_ericsson_MAIN_2 | no | 0 | 0 | refused: HEVC decode: multiple slices per picture (pixel decode covers one slice segment per picture) |
| WPP_F_ericsson_MAIN10_2 | no | 0 | 0 | refused: HEVC decode: multiple slices per picture (pixel decode covers one slice segment per picture) |
| WPP_F_ericsson_MAIN_2 | no | 0 | 0 | refused: HEVC decode: multiple slices per picture (pixel decode covers one slice segment per picture) |
| WP_A_MAIN10_Toshiba_3 | yes | 1 | 1 | ok |
| WP_A_Toshiba_3 | yes | 1 | 1 | ok |
| WP_B_Toshiba_3 | yes | 4 | 4 | ok |
| WP_MAIN10_B_Toshiba_3 | yes | 4 | 4 | ok |
| cip_B_NEC_3 | yes | 1 | 1 | ok |
| ipcm_A_NEC_3 | yes | 1 | 1 | ok |
| ipcm_B_NEC_3 | yes | 1 | 1 | ok |
| ipcm_C_NEC_3 | yes | 1 | 1 | ok |
| ipcm_D_NEC_3 | yes | 1 | 1 | ok |
| ipcm_E_NEC_2 | yes | 1 | 1 | ok |

## Left

- Multiple slices per picture (30 streams, including the WPP_* streams, which are refused for having several slices per picture) and dependent slices: refused. Needs slice address handling and per-slice availability in `PicState`.
- Tiles (16 streams): refused. Needs the tile scan (6.5.1) in CTB addressing and in availability.
- Unequal luma and chroma bit depth (TSUNEQBD): refused.
- Inter prediction: only a placeholder. Only intra pictures are checked, so P/B pictures are not verified.
- `cu_transquant_bypass` and PCM: the ipcm_A..E streams are exact, which covers PCM. No stream was checked specifically for transquant bypass.
- `dec/deblock.rs` and `dec/sao.rs` are verified by the exact conformance pictures and their unit tests; no separate per-tool stream check exists.

