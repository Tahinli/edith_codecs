# lane hevcrext: ec-h265 decodes HEVC Range Extensions

Branch lane/hevcrext (from main 26bafe4e), local commits only, not pushed.

## What decodes

- Chroma formats 4:0:0, 4:2:0, 4:2:2, 4:4:4 at 8, 10 and 12 bits (luma and chroma depth may differ).
  Monochrome output has empty `cb`/`cr` planes (`chroma_format_idc` 0); internally the decoder keeps
  dummy chroma planes and strips them in `SharedPic::snapshot`.
- RExt tools: transform_skip rotation, transform_skip context, implicit and explicit RDPCM,
  log2_max_transform_skip_block_size, persistent_rice_adaptation (StatCoeff travels with the
  `Contexts`, so WPP and dependent-slice sync cover it), cross_component_prediction,
  chroma_qp_offset_list, intra_smoothing_disabled, high_precision_offsets (weights parse and
  offset scaling), log2_sao_offset_scale, 4:4:4 chroma intra (4 chroma modes for NxN, 4x4 chroma TUs,
  neighbour filtering of chroma, scan index of 8x8 chroma), 32x32 chroma scaling matrices for 4:4:4,
  inter/MC/weighted prediction up to 12 bits, deblocking and SAO for 4:2:2/4:4:4.
- 4:2:2 (intra mode table, split chroma TU, deblock/SAO) was already exact from lane hevc422 and is
  re-verified here at 8/10/12 bits.

Output format: `DecodedPicture` already carries `chroma_format_idc`, `bit_depth`, `bit_depth_c` and `u16`
planes; no new type was needed.

## Refusals (by name, `Error::Unsupported`)

- `extended_precision_processing_flag`, `cabac_bypass_alignment_enabled_flag` (costly: new coefficient
  range, binarisation and aligned bypass engine; only the 16-bit/high-throughput profiles use them).
- bit depth above 12 (13 to 16), `separate_colour_plane_flag`, SCC, multilayer and 3D extensions (unchanged).

## Verification

- JCT-VC RExt conformance (`scripts/fetch-vectors.sh hevc-rext`, 49 zips, sha256 pinned in
  `scripts/vectors.sha256`; test `conformance_rext`): 35 of 49 streams accepted, 969/969 pictures exact against
  the SEI picture hash or the package's whole-output MD5. The other 14 are refused by name:
  EXTPREC_* (8 streams, extended precision or bypass alignment), GENERAL_16b_* (3), WAVETILES (16-bit),
  WPP_..._BYPASS_ALIGN_1 and WPP_..._EXT_PREC_1 (14-bit). Includes CCP 8/10/12, ExplicitRdpcm A/B,
  PERSIST_RPARAM_A, TSCTX 8/10/12 (I and inter), QMATRIX_A (4:4:4), SAO_A, IPCM A/B, GENERAL 8/10/12-bit
  400/420/422/444, Main_422_10 A/B, ADJUST_IPRED_ANGLE_A, Bitdepth_A/B, WPP/tiles 4:4:4 8-bit.
- Synthetic libx265 streams (`scripts/gen-hevc-rext-fixtures.sh`, committed in `tests/data/rext` with the
  MD5 of ffmpeg's decode; test `rext_generated`, needs no ffmpeg at run time): yuv444p, yuv444p10le,
  yuv444p12le, yuv422p, yuv422p10le, yuv422p12le, gray, gray10le, gray12le with B-frames and weighted
  prediction, plus transform-skip/rect/AMP/SAO, lossless (transquant bypass) and WPP+constrained-intra
  variants: 25 of 25 streams byte-identical to ffmpeg. x265 does not emit RDPCM, cross-component or persistent
  Rice, so those tools are covered by the conformance vectors only. (x265 `slices=2` output is malformed at
  this size, ffmpeg itself reports "Overread slice header", so it is not used.)
- Main and Main10 output and the hash pins are unchanged: the whole existing ec-h265 suite passes.

## Gate

- `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `EC_REQUIRE_FIXTURES=1 cargo test -p ec-h265 --release`: all test binaries, 0 failures
  (92 unit tests, conformance 15, conformance_intra 2, conformance_rext 2, conformance_rext422 2,
  refusals 14, rext_generated 1, ...). `cargo test -p ec-h265-syntax --release`: all pass.

## Notes

- Shared encoder-side files touched minimally: `cabac.rs` (4 new context rows appended, `stat_coeff` in
  `Contexts`), `intra.rs` (`predict_ext`, `scan_index_fmt`; `predict` unchanged), `residual.rs`
  (`decode_residual_ext`; `decode_residual` is a wrapper). No encoder file was edited.
- `ec-h265-syntax`: `pred_weight_table` offset half range honours high_precision_offsets; 32x32 chroma scaling
  matrices for 4:4:4.
- Test memory: the conformance harness feeds the decoder per NAL and drains frames, since the 2560x1600
  4:4:4 12-bit ExplicitRdpcm_B stream (150 frames) otherwise held 3.8 GB.
