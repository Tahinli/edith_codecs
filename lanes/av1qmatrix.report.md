# lane-av1-qmatrix: `using_qmatrix` dequantisation lift

Base 73f3ac64, worktree `~/Documents/Code/Rust/edith_codecs-av1qm`, branch
`lane-av1-qmatrix`. Owner: the using_qmatrix dequant path only.

## What landed

`using_qmatrix` + `qm_y`/`qm_u`/`qm_v` (spec 5.9.12) now select per-plane
inverse quantisation matrices, ported bug-for-bug from libaom
(`av1/common/quant_common.c`, `av1/decoder/decodetxb.c`). The frame-header
refusal from lane-av1txr-r2 is retired; `stream.rs` records the frame's
levels instead.

New module `crates/ec-av1/src/qm.rs`:

- `IWT`: libaom `iwt_matrix_ref[NUM_QM_LEVELS-1][2][QM_TOTAL_SIZE]` verbatim
  (100 320 u8 = 15 levels x [luma, chroma] x 3344), extracted mechanically
  from the oracle source with the `/* Size ... */` comment markers cross-checking
  the chunk order. Level 15 absent (libaom stores NULL = flat for it).
- `iwt_matrix(level, chroma, w, h)` = `av1_get_iqmatrix` narrowed to the
  decode path: level >= 15 -> `None`; 64-axis clamps
  (`av1_get_adjusted_tx_size`) -> the smaller shape's matrix
  (64x64/64x32/32x64 -> 32x32, 64x16 -> 32x16, 16x64 -> 16x32).
- `dqv_at` = `get_dqv` (`decodetxb.c:54-60`): `dqv = ((m * dqv) + 16) >> 5`
  (`AOM_QM_BITS = 5`), applied to the DC and the AC quantizer alike.

Wiring (mirror of libaom's `setup_segmentation_dequant` + `av1_use_qmatrix`):

- Levels are per FRAME, read straight off the parsed 4-bit fields -- the
  parsed value IS the table level (libaom `qmatrix_level_y/u/v`,
  decodeframe.c:1902-1917); the `aom_get_qmlevel(base_q_idx, ...)` formula is
  encoder-side only. When `using_qmatrix == 0` the parsed fields read 0 but
  are not coded: levels are parked at 15 (flat), exactly libaom's sentinel.
  Recorded per frame at the old refusal site (`set_qm_levels`).
- Per block (`block_iqmatrix`, decode.rs): `None` (flat) when the frame is
  flat, when the transform is 1D/identity (`TxType::is_2d_transform`, libaom
  `is_2d_transform`: `V_DCT`/`H_DCT`/`IDTX` read the NULL slot), or when the
  block is lossless (segment-adjusted quantizer index 0 with all five plane
  deltas zero -- libaom `av1_use_qmatrix`'s `!xd->lossless[seg]`, using the
  same `block_q_idx` every other dequant call site reads).
- Plumbing: `qm: Option<Iqm>` threaded through
  `dequant_and_inverse_typed_wh` -> `quant::dequant_wh_into` and captured in
  `TxParams` (selected on the parse thread, so the recon worker scales
  identically). The encoder model, the lossless WHT path and the
  transform/quant tests pass `None` -- they model matrix-free streams.

## The subtle half: the matrix index is COLUMN-major

`get_dqv` indexes `iqmatrix[coeff_idx]` where `coeff_idx` is a position in
libaom's txb coefficient space, and that space is COLUMN-major (h-strided):
libaom's `mcol_scan_8x4` is the identity `0,1,2,...`, i.e. position `p`
strides DOWN the columns of the h-tall block. Our levels grid is row-major
(w-wide), so `dqv_at` re-strides: `idx = col * stride + row`, with
`stride = active height` (32 for the 64-clamps' square active area). My first
port used the row-major raster and was off by transposition at every
non-4x4 unit: the 4x4 matrices are symmetric, so twenty units matched and
masked it; the first rect unit (8x4 chroma) diverged, and the
instrumented-oracle coefficient diff (EC_DQCOEFF, 442 units, 27 diverging --
all rects plus 8x8+ squares) pinned the mapping. After the fix the whole
repro stream decodes byte-identical to the oracle/ffmpeg.

## Non-vacuity

`QM_SCALED_UNITS` (`quant.rs`, feature-gated like every counter): transform
units whose dequantisation actually scaled a nonzero level through a matrix.
Witness asserts > 0 on the qm=1 arm and == 0 on the qm-off control, plus the
parsed headers carrying `using_qmatrix = true` / `false` and levels 5/5/5.

## Gates (this tree, quoted)

- Witness (flipped from the old refusal gate, which self-described as
  "flip this gate to a witness"):
  `a_real_aomenc_quantisation_matrix_stream_decodes_pixel_exact`:
  qm-off control 3 frames pixel-exact vs ffmpeg (0 matrix-scaled units),
  qm-on 3 frames pixel-exact vs ffmpeg (420 matrix-scaled units, levels
  5/5/5). PASS.
- Fail-pre-fix, measured: `decode_probe` built from a pristine
  `git archive` of 73f3ac64 refuses the same qm=1 stream:
  `REFUSED: unsupported: AV1 decode_stream (a frame using quantisation
  matrices (using_qmatrix=1): ...)`. The lane tree decodes it byte-exact.
- `qm::` unit tests: table anchors (level-0 luma/chroma 4x4 rows read off the
  C file), luma != chroma at the same level, level/shape selection over all
  15 levels x 14 shapes x 2 kinds, 64-clamp reuse (pointer identity with the
  adjusted shape), `dqv_at` scaling/rounding/column-major transposition
  (the pos-1 vs pos-8 corner where row- and column-major disagree).
  5 passed.
- Extra local probes, byte-exact vs the oracle/ffmpeg decode:
  `gradients` 256x192 cq40 cpu1 qm=1 (3 frames), and a genuine 10-bit
  `--bit-depth=10` qm=1 stream whose two frames carry levels 5/5/5 AND
  7/7/7 (base_q_idx 29 / 116) -- second level table exercised at 10-bit.
- Regression arms, all green on this tree: `refusal_inventory` (+15),
  `a_non_420_subsampled_sequence_header_is_refused_by_name` (4:4:4 still
  refused by name), `a_real_libaom_monochrome_key_frame_decodes_pixel_exact`
  (mono witness), `a_reader_told_not_to_adapt` (cdf-disabled),
  `a_real_aomenc_mixed_lossless_segment_frame_is_refused_by_name`
  (segmentation-override), `the_hunger_games_*` hg fixtures,
  `a_real_aomenc_10bit_stream_decodes_pixel_exact`,
  `a_real_aomenc_10bit_inter_sequence_decodes_pixel_exact`,
  `a_10bit_key_frame_with_skipped_8x8_intra_leaves_that_split_their_transform_decodes_luma_exact`.
- `quant::` + `transform::` scoped tests: 29 passed (buffered-vs-per-coeff
  dequant, WHT lossless, tx-type round trips -- all through the `None` path).
- `cargo check -p ec-av1 --all-targets`: 0 warnings.

## Class sweep

Every residual-dequant site goes through `dequant_wh_into`; all 21 decode
call sites + 3 `TxParams` constructions now pass a selected matrix (or the
flat `None`), so no dequant can silently ignore a qm frame. Non-decode
callers kept on `None` (they model matrix-free streams): `encode.rs`'s two
reconstruction-model sites, `dequant_and_inverse_typed`/`dequant_and_inverse`
(test/model entry points), `dequant_coeff`/`dequant_coeff_wh` (spec 7.12.3
per-coefficient helpers, no decode caller), and the lossless WHT4x4 (a
lossless frame can never select a matrix). Chroma-from-luma, palette,
prediction and the loop filters never touch the quantizer.

## Full suite

Handed to Main for the VPS run per standing order (local runs kept to named
tests and one-fixture probes). Result to be appended by the verifier.
