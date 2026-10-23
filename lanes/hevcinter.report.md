# lane hevcinter: inter pixels, every picture checked

## Goal

Every picture (I, P, B) of every accepted conformance stream decodes MD5-exact. Inter prediction is wired into the pixel path, the placeholder copy is gone, and `tests/conformance_intra.rs` checks every output picture.

## Base

- Lane branch `lane/hevcinter`, created from `main` at `5d7956ca`.
- Main merged in at `bd9bec14` (lane/hevcslices: slices, dependent segments, tiles, WPP, unequal bit depth; 147/147 accepted).

## What changed

- `crates/ec-h265/src/dec/inter.rs`: rewritten. `InterRefs` holds the final (post-deblock, post-SAO) planes of every `RefPicList0`/`RefPicList1` entry from the DPB, plus `ExplicitWeights::from_table` when the slice has a `pred_weight_table` (weighted_pred for P, weighted_bipred for B). `add_residuals` adds a coding unit's residual after its prediction.
- `crates/ec-h265/src/dec/cu_syntax.rs`: `parse_slice_into` takes `Option<&InterRefs>` in place of the single placeholder reference. Each inter prediction unit (and each skipped CU) calls `predict_pu` right after its motion is derived, which calls `mc::predict_pu`. Residuals are added after prediction. The placeholder is removed.
- `crates/ec-h265/src/dec/mod.rs`: `decode_slice` builds `InterRefs` from the slice's lists.
- `crates/ec-h265/src/dec/pic.rs`: `planes_mut()` gives `PlaneMut` views for motion compensation.
- `crates/ec-h265/tests/conformance_intra.rs`: checks every output picture, not only intra ones. Hashes: MD5 (hash_type 0), CRC (1) and checksum (2), all per D.3.19. Streams with no hash SEI are checked against a reference YUV named by `EC_H265_REF_YUV_DIR`, cropped to the conformance window. The table shows decode time per stream.
- Constrained intra: already implemented in `recon.rs` (`constrained_intra_pred` gates neighbour availability on `info.intra`). Not changed.
- Missing references (8.3.3): already handled by `rps.rs` (generated grey pictures enter the DPB). Not changed.

## Control experiment

With motion compensation disabled (an env switch, since removed), 326 pictures are exact. With it enabled, 8198 of 9203 are. The inter path is what makes the P and B pictures exact.

## Reference YUVs

The 35 streams with no hash SEI were checked against ffmpeg output (`ffmpeg -i X -fps_mode passthrough -f rawvideo`). ffmpeg is not normative. It agrees exactly with the decoder on every hashless stream except SAODBLK_A, SAODBLK_B and VPSSPSPPS_A (below). Hash streams are checked against the SEI, not ffmpeg.

## Verify

| Command | Result |
|---|---|
| `scripts/remote-run.sh hevcinter -- cargo clippy -p ec-h265 -p ec-h265-syntax --all-targets -- -D warnings` | pass (exit 0) |
| `scripts/remote-run.sh hevcinter -- cargo test -p ec-h265 -p ec-h265-syntax --release` | pass, all binaries 0 failed (ec_h265 lib 82; conformance 15; conformance_intra 1; cu_syntax 4; dec_meta 4; dec_shell 13; decode 5; encode 4; perf 3; rdoq_cache_hash 3; ec-h265-syntax 29, fuzz 2, sei_sweep 1). |
| `scripts/remote-run.sh hevcinter -- cargo check --workspace --all-targets` | pass (exit 0); warnings only in ec-av1 |

The conformance test (`EC_H265_REF_YUV_DIR` set to the reference YUVs) reports:

**total: 9203 pictures checked, 8198 exact, 147/147 streams accepted.**

Streams not exact:

| Stream | Checked | Exact | First mismatch |
|---|---|---|---|
| SAODBLK_A_MainConcept_4 | 500 | 0 | poc=0 Cb x=455 y=369 (against ffmpeg). Ours is 122, ffmpeg 129. |
| SAODBLK_B_MainConcept_4 | 500 | 1 | poc=1 Cb x=371 y=162 (against ffmpeg). |
| VPSSPSPPS_A_MainConcept_1 | 6 | 0 | poc=0 Y x=77 y=0. Not conclusive: ffmpeg logs "PPS id out of range" and outputs 2 frames, not 6, so its reference is incomplete. |

Diagnosis of SAODBLK_A (not yet fixed): the SAO trace shows our edge category at that Cb sample is 4 (local maximum, offset -7). With SAO disabled our sample is 129, matching ffmpeg's final value. So the difference is in the SAO decision on deblocked neighbours, not in the offset parse. The neighbours are in the same slice and tile, and the CTB is 16x16 (the sample is in the last chroma column of its CTB). The neighbour (456,368) is a horizontal chroma edge sample, so a deblocking difference there is the leading suspect. Not confirmed. The EO table and boundary rules were checked against the spec.

Decode time (release, server, per stream row, includes the hash or reference comparison): AMP_F_Hisilicon_3 (1920x1080, 100 frames) 5.45 s; DSLICE_A_HHI_5 (1920x1080, 50 frames) 3.33 s; the whole conformance run 136 s.

## Per-stream table

Columns: stream, accepted, pictures checked, pictures exact, detail, decode seconds (decode plus the comparison).

| stream | accepted | checked | exact | detail | decode_s |
|---|---|---|---|---|---|
| AMP_A_Samsung_7 | yes | 17 | 17 | ok (against the reference YUV) | 1.79s |
| AMP_B_Samsung_7 | yes | 17 | 17 | ok (against the reference YUV) | 2.93s |
| AMP_D_Hisilicon | yes | 100 | 100 | ok (against the reference YUV) | 6.40s |
| AMP_E_Hisilicon | yes | 100 | 100 | ok (against the reference YUV) | 6.48s |
| AMP_F_Hisilicon_3 | yes | 100 | 100 | ok (against the reference YUV) | 5.45s |
| AMVP_A_MTK_4 | yes | 30 | 30 | ok (against the reference YUV) | 0.06s |
| AMVP_B_MTK_4 | yes | 41 | 41 | ok (against the reference YUV) | 0.08s |
| AMVP_C_Samsung_7 | yes | 60 | 60 | ok (against the reference YUV) | 1.20s |
| BUMPING_A_ericsson_1 | yes | 49 | 49 | ok | 0.41s |
| CAINIT_A_SHARP_4 | yes | 50 | 50 | ok | 0.61s |
| CAINIT_B_SHARP_4 | yes | 50 | 50 | ok | 0.60s |
| CAINIT_C_SHARP_3 | yes | 50 | 50 | ok | 0.63s |
| CAINIT_D_SHARP_3 | yes | 50 | 50 | ok | 0.66s |
| CAINIT_E_SHARP_3 | yes | 50 | 50 | ok | 0.64s |
| CAINIT_F_SHARP_3 | yes | 50 | 50 | ok | 0.64s |
| CAINIT_G_SHARP_3 | yes | 50 | 50 | ok | 0.65s |
| CAINIT_H_SHARP_3 | yes | 50 | 50 | ok | 0.68s |
| CIP_A_Panasonic_3 | yes | 2 | 2 | ok | 0.01s |
| CIP_C_Panasonic_2 | yes | 2 | 2 | ok | 0.01s |
| CONFWIN_A_Sony_1 | yes | 60 | 60 | ok (against the reference YUV) | 0.36s |
| DBLK_A_MAIN10_VIXS_4 | yes | 8 | 8 | ok (against the reference YUV) | 0.03s |
| DBLK_A_SONY_3 | yes | 30 | 30 | ok (against the reference YUV) | 0.45s |
| DBLK_B_SONY_3 | yes | 30 | 30 | ok (against the reference YUV) | 0.33s |
| DBLK_C_SONY_3 | yes | 30 | 30 | ok (against the reference YUV) | 0.72s |
| DBLK_D_VIXS_2 | yes | 8 | 8 | ok (against the reference YUV) | 0.13s |
| DBLK_E_VIXS_2 | yes | 8 | 8 | ok (against the reference YUV) | 0.14s |
| DBLK_F_VIXS_2 | yes | 8 | 8 | ok (against the reference YUV) | 0.15s |
| DBLK_G_VIXS_2 | yes | 8 | 8 | ok (against the reference YUV) | 0.17s |
| DELTAQP_A_BRCM_4 | yes | 96 | 96 | ok | 6.22s |
| DELTAQP_B_SONY_3 | yes | 30 | 30 | ok (against the reference YUV) | 1.20s |
| DELTAQP_C_SONY_3 | yes | 30 | 30 | ok (against the reference YUV) | 0.69s |
| DSLICE_A_HHI_5 | yes | 50 | 50 | ok | 3.33s |
| DSLICE_B_HHI_5 | yes | 50 | 50 | ok | 3.58s |
| DSLICE_C_HHI_5 | yes | 50 | 50 | ok | 3.45s |
| ENTP_A_Qualcomm_1 | yes | 24 | 24 | ok | 3.71s |
| ENTP_B_Qualcomm_1 | yes | 24 | 24 | ok | 1.45s |
| ENTP_C_Qualcomm_1 | yes | 24 | 24 | ok | 1.52s |
| EXT_A_ericsson_4 | yes | 3 | 3 | ok | 0.03s |
| FILLER_A_Sony_1 | yes | 60 | 60 | ok (against the reference YUV) | 0.35s |
| HRD_A_Fujitsu_3 | yes | 96 | 96 | ok | 0.42s |
| INITQP_A_Sony_1 | yes | 60 | 60 | ok (against the reference YUV) | 0.34s |
| INITQP_B_Main10_Sony_1 | yes | 90 | 90 | ok (against the reference YUV) | 0.54s |
| IPRED_A_docomo_2 | yes | 20 | 20 | ok | 0.63s |
| IPRED_B_Nokia_3 | yes | 1 | 1 | ok | 0.10s |
| IPRED_C_Mitsubishi_3 | yes | 4 | 4 | ok | 0.18s |
| LS_A_Orange_2 | yes | 9 | 9 | ok | 0.12s |
| LS_B_Orange_4 | yes | 25 | 25 | ok | 0.95s |
| LTRPSPS_A_Qualcomm_1 | yes | 500 | 500 | ok (against the reference YUV) | 1.62s |
| MAXBINS_A_TI_5 | yes | 2 | 2 | ok (against the reference YUV) | 0.03s |
| MAXBINS_B_TI_5 | yes | 3 | 3 | ok (against the reference YUV) | 0.02s |
| MAXBINS_C_TI_5 | yes | 3 | 3 | ok (against the reference YUV) | 0.03s |
| MERGE_A_TI_3 | yes | 8 | 8 | ok | 0.04s |
| MERGE_B_TI_3 | yes | 8 | 8 | ok | 0.04s |
| MERGE_C_TI_3 | yes | 8 | 8 | ok | 0.04s |
| MERGE_D_TI_3 | yes | 8 | 8 | ok | 0.04s |
| MERGE_E_TI_3 | yes | 8 | 8 | ok | 0.04s |
| MERGE_F_MTK_4 | yes | 41 | 41 | ok (against the reference YUV) | 0.08s |
| MERGE_G_HHI_4 | yes | 2 | 2 | ok | 0.06s |
| MVCLIP_A_qualcomm_3 | yes | 5 | 5 | ok | 0.06s |
| MVDL1ZERO_A_docomo_4 | yes | 500 | 500 | ok | 6.75s |
| MVEDGE_A_qualcomm_3 | yes | 17 | 17 | ok | 0.12s |
| NUT_A_ericsson_5 | yes | 34 | 34 | ok | 0.22s |
| NoOutPrior_A_Qualcomm_1 | yes | 40 | 40 | ok | 0.48s |
| NoOutPrior_B_Qualcomm_1 | yes | 47 | 47 | ok | 0.56s |
| OPFLAG_A_Qualcomm_1 | yes | 500 | 500 | ok | 1.79s |
| OPFLAG_B_Qualcomm_1 | yes | 98 | 98 | ok | 3.34s |
| OPFLAG_C_Qualcomm_1 | yes | 96 | 96 | ok | 1.14s |
| PICSIZE_A_Bossen_1 | yes | 10 | 10 | ok | 2.88s |
| PICSIZE_B_Bossen_1 | yes | 10 | 10 | ok | 2.82s |
| PICSIZE_C_Bossen_1 | yes | 10 | 10 | ok | 0.87s |
| PICSIZE_D_Bossen_1 | yes | 10 | 10 | ok | 0.65s |
| PMERGE_A_TI_3 | yes | 8 | 8 | ok | 0.04s |
| PMERGE_B_TI_3 | yes | 8 | 8 | ok | 0.04s |
| PMERGE_C_TI_3 | yes | 8 | 8 | ok | 0.04s |
| PMERGE_D_TI_3 | yes | 8 | 8 | ok | 0.04s |
| PMERGE_E_TI_3 | yes | 8 | 8 | ok | 0.04s |
| POC_A_Bossen_3 | yes | 5 | 5 | ok (against the reference YUV) | 0.05s |
| PPS_A_qualcomm_7 | yes | 81 | 81 | ok | 1.51s |
| PS_B_VIDYO_3 | yes | 25 | 25 | ok | 0.06s |
| RAP_A_docomo_6 | yes | 86 | 86 | ok (against the reference YUV) | 0.32s |
| RAP_B_Bossen_2 | yes | 80 | 80 | ok | 0.30s |
| RPLM_A_qualcomm_4 | yes | 300 | 300 | ok | 1.06s |
| RPLM_B_qualcomm_4 | yes | 300 | 300 | ok | 0.98s |
| RPS_A_docomo_5 | yes | 44 | 44 | ok (against the reference YUV) | 0.22s |
| RPS_B_qualcomm_5 | yes | 300 | 300 | ok | 0.98s |
| RPS_C_ericsson_5 | yes | 40 | 40 | ok | 0.27s |
| RPS_D_ericsson_6 | yes | 68 | 68 | ok | 0.45s |
| RPS_E_qualcomm_5 | yes | 300 | 300 | ok | 1.41s |
| RPS_F_docomo_2 | yes | 50 | 50 | ok (against the reference YUV) | 0.23s |
| RQT_A_HHI_4 | yes | 2 | 2 | ok | 0.06s |
| RQT_B_HHI_4 | yes | 2 | 2 | ok | 0.06s |
| RQT_C_HHI_4 | yes | 2 | 2 | ok | 0.07s |
| RQT_D_HHI_4 | yes | 2 | 2 | ok | 0.06s |
| RQT_E_HHI_4 | yes | 2 | 2 | ok | 0.08s |
| RQT_F_HHI_4 | yes | 2 | 2 | ok | 0.07s |
| RQT_G_HHI_4 | yes | 2 | 2 | ok | 0.06s |
| SAODBLK_A_MainConcept_4 | yes | 500 | 0 | first mismatch poc=0 plane=Cb x=455 y=369 (reference YUV, cropped) | 7.72s |
| SAODBLK_B_MainConcept_4 | yes | 500 | 1 | first mismatch poc=1 plane=Cb x=371 y=162 (reference YUV, cropped) | 10.68s |
| SAO_A_MediaTek_4 | yes | 60 | 60 | ok | 0.16s |
| SAO_B_MediaTek_5 | yes | 60 | 60 | ok | 0.68s |
| SAO_C_Samsung_5 | yes | 5 | 5 | ok (against the reference YUV) | 0.57s |
| SAO_D_Samsung_5 | yes | 5 | 5 | ok (against the reference YUV) | 0.63s |
| SAO_E_Canon_4 | yes | 16 | 16 | ok (against the reference YUV) | 0.09s |
| SAO_F_Canon_3 | yes | 16 | 16 | ok (against the reference YUV) | 0.06s |
| SAO_G_Canon_3 | yes | 16 | 16 | ok (against the reference YUV) | 0.06s |
| SAO_H_Parabola_1 | yes | 4 | 4 | ok (against the reference YUV) | 0.04s |
| SDH_A_Orange_4 | yes | 2 | 2 | ok | 0.41s |
| SLICES_A_Rovi_3 | yes | 9 | 9 | ok | 0.10s |
| SLIST_A_Sony_5 | yes | 65 | 65 | ok | 0.89s |
| SLIST_B_Sony_9 | yes | 65 | 65 | ok | 0.95s |
| SLIST_C_Sony_4 | yes | 65 | 65 | ok | 0.92s |
| str | yes | 65 | 65 | ok | 0.99s |
| SLPPLP_A_VIDYO_2 | yes | 33 | 33 | ok | 0.08s |
| STRUCT_A_Samsung_7 | yes | 60 | 60 | ok (against the reference YUV) | 0.76s |
| STRUCT_B_Samsung_7 | yes | 60 | 60 | ok (against the reference YUV) | 0.85s |
| TILES_A_Cisco_2 | yes | 100 | 100 | ok (against the reference YUV) | 3.66s |
| TILES_B_Cisco_1 | yes | 100 | 100 | ok (against the reference YUV) | 3.70s |
| TMVP_A_MS_3 | yes | 17 | 17 | ok | 0.06s |
| TSCL_A_VIDYO_5 | yes | 73 | 73 | ok | 0.19s |
| TSCL_B_VIDYO_4 | yes | 73 | 73 | ok | 0.21s |
| TSKIP_A_MS_3 | yes | 17 | 17 | ok | 0.27s |
| TSUNEQBD_A_MAIN10_Technicolor_2 | yes | 8 | 8 | ok | 0.33s |
| TUSIZE_A_Samsung_1 | yes | 5 | 5 | ok (against the reference YUV) | 1.51s |
| VPSID_A_VIDYO_2 | yes | 33 | 33 | ok | 0.08s |
| VPSSPSPPS_A_MainConcept_1 | yes | 6 | 0 | first mismatch poc=0 plane=Y x=77 y=0 (reference YUV, cropped) | 0.13s |
| WPP_A_ericsson_MAIN10_2 | yes | 48 | 48 | ok | 0.16s |
| WPP_A_ericsson_MAIN_2 | yes | 48 | 48 | ok | 0.15s |
| WPP_B_ericsson_MAIN10_2 | yes | 48 | 48 | ok | 0.18s |
| WPP_B_ericsson_MAIN_2 | yes | 48 | 48 | ok | 0.17s |
| WPP_C_ericsson_MAIN10_2 | yes | 48 | 48 | ok | 0.20s |
| WPP_C_ericsson_MAIN_2 | yes | 48 | 48 | ok | 0.16s |
| WPP_D_ericsson_MAIN10_2 | yes | 48 | 48 | ok | 0.03s |
| WPP_D_ericsson_MAIN_2 | yes | 48 | 48 | ok | 0.03s |
| WPP_E_ericsson_MAIN10_2 | yes | 48 | 48 | ok | 0.06s |
| WPP_E_ericsson_MAIN_2 | yes | 48 | 48 | ok | 0.06s |
| WPP_F_ericsson_MAIN10_2 | yes | 48 | 48 | ok | 0.07s |
| WPP_F_ericsson_MAIN_2 | yes | 48 | 48 | ok | 0.07s |
| WP_A_MAIN10_Toshiba_3 | yes | 256 | 256 | ok | 0.62s |
| WP_A_Toshiba_3 | yes | 256 | 256 | ok | 0.47s |
| WP_B_Toshiba_3 | yes | 256 | 256 | ok | 0.60s |
| WP_MAIN10_B_Toshiba_3 | yes | 256 | 256 | ok | 0.66s |
| cip_B_NEC_3 | yes | 5 | 5 | ok | 0.02s |
| ipcm_A_NEC_3 | yes | 1 | 1 | ok | 0.01s |
| ipcm_B_NEC_3 | yes | 1 | 1 | ok | 0.01s |
| ipcm_C_NEC_3 | yes | 1 | 1 | ok | 0.01s |
| ipcm_D_NEC_3 | yes | 1 | 1 | ok | 0.01s |
| ipcm_E_NEC_2 | yes | 1 | 1 | ok | 0.01s |
