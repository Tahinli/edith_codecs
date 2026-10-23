# lane hevc422: ec-h265 HEVC 4:2:2 (RExt)

Base: 00054596 (main). Branch lane/hevc422, local commit only, not pushed.

## Outcome (attempt 3, final)

4:2:2 is bit-exact at 8 and 10 bit against ffmpeg; the refusal is lifted. 4:4:4 and 12-bit stay refused.

Root cause of the intra chroma mismatch: the MODE_422 table (Table 8-3) was the pre-final one.
In the final table the horizontal-ish modes double their angle for the half-width chroma grid:
luma mode 11 (angle -2) maps to 12 (-5), and 14 (-13) maps to 17 (-26). The old table had
11->11 and 14->16. The earlier "reference matches mode 17" observation was therefore a table
entry, not a neighbour-sample bug; the earlier one-entry experiment (14->17 only) fixed that
block but 11->12 was still missing, which is why the Sony vectors did not move. With both entries
fixed every 4:2:2 stream is exact, so the deblock/SAO suspicion for Main_422_10_A frame 7 was
the same prediction fault propagating through the GOP, not a filter bug.

Final MODE_422 (cu_syntax.rs): 0,1,2,2,2,2,3,5,7,8,10,12,13,15,17,18,19,20,21,22,23,23,24,24,
25,25,26,27,27,28,28,29,29,30,31.

## Changes

- cu_syntax.rs: MODE_422 entries 11 and 14.
- dec/mod.rs: check_sps no longer refuses chroma_format_idc 2 (4:4:4 and 4:0:0 still refused).
- tests/conformance_rext422.rs (new): committed 64x64 4:2:2 10-bit stream vs ffmpeg at run time;
  Sony RExt vectors and generated clips vs ffmpeg output, skipped when the gitignored fixtures
  are absent (EC_H265_REXT_DIR).
- tests/refusals.rs: the 4:2:2 refusal test removed.

## Per-vector table (byte-exact against ffmpeg, all frames, cropped to the output size)

| vector | result |
|---|---|
| GENERAL_10b_422_RExt_Sony_1 (400x384) | 2/2 exact |
| Main_422_10_A_RExt_Sony_2 (1920x1080) | 9/9 exact |
| Main_422_10_B_RExt_Sony_2 (2560x1600) | 1/1 exact |
| GENERAL_12b_422_RExt_Sony_1 | refused (12-bit) |
| i422_8, g422_8, i422_10, g422_10, g422_8_tools, g422_10_tools | 50/50 exact each |

## Fixtures (gitignored, fixtures/vectors/hevc-rext and hevc-rext-gen)

- GENERAL_10b_422_RExt_Sony_1.zip md5 3439a4e30c9ba28a417f64aa104b2afb
- GENERAL_12b_422_RExt_Sony_1.zip md5 b898eeb1dae36fa33febc4e66a93d861
- Main_422_10_A_RExt_Sony_2.zip md5 fa324ed413f5a1cab239e8cf8a856ac8
- Main_422_10_B_RExt_Sony_2.zip md5 526b496564a10df1b9c2a239ad16d1de
- ffmpeg reference md5 (rawvideo): GENERAL_10b ea5dfb01c95bffb169cd78e549bd39ab, Main_422_10_A
  66530b83be6647f87efbb2b9ae6e1e28, Main_422_10_B fd163bcbdc24792a23781ee7aace74eb,
  GENERAL_12b d4f51ab5405659ad90e788083bcfc18d.
- Generated clips (ffmpeg x265, yuv422p and yuv422p10le, intra and GOP, SAO/AMP/weightp): i422_8,
  g422_8, i422_10, g422_10, g422_8_tools, g422_10_tools, i422_8_plain, i422_8_nosao, i422_8_nodbk.

## Left

- 4:4:4 and the refused RExt tools (transform_skip rotation/context, RDPCM, extended precision,
  intra_smoothing_disabled, high_precision_offsets, persistent_rice_adaptation,
  cabac_bypass_alignment, log2_max_transform_skip_block_size, cross_component_prediction,
  log2_sao_offset_scale) are still refused by name; 12-bit too.
