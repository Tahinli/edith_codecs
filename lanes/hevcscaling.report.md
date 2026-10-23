# hevcscaling: HEVC scaling lists

Base: `main` at b03ae4e6.

## Goal
Lift the scaling-list refusal in ec-h265-syntax, parse and write `scaling_list_data()`,
derive ScalingFactor, and dequantise with it. Unblocks SLIST_A_Sony_5, SLIST_B_Sony_9,
SLIST_C_Sony_4 and `str` (SLIST_D_Sony_9/str.bin) in the JCT-VC sweep.

## What changed
- `ec-h265-syntax/src/scaling.rs` (new): `ScalingList` (`scaling_list_data()` parse and
  write, with `pred_mode`/`pred_matrix_id_delta` copy from reference and default at delta 0,
  DC for sizeId 2/3, 32x32 matrixId step 3). Default lists from Tables 7-5 and 7-6.
  `ScalingFactors` derives the raster ScalingFactor per sizeId/matrixId, with DC upsampling,
  and `for_picture` picks PPS over SPS over defaults.
- `ps.rs`: `Sps::scaling_list` and `Pps::scaling_list` (explicit data, `None` = not sent).
  Both refusals removed; to_rbsp writes the lists. `lib.rs` re-exports the new types.
- `ec-h265/src/transform.rs`: `dequantize_with` / `dequant_weighted`. Flat 16 is
  bit-identical to the old path; `dequantize` and `dequant_level` keep their signatures.
- `ec-h265/src/decode.rs`: intra CTU decoder takes the picture's factors. Weight is flat
  when transform_skip and nTbS > 4 (8.6.4.2). Chroma uses matrixId = cIdx.
- `ec-h265/src/decode.rs` (WPP fix, found by the new test): the intra CTU loop read
  end_of_subset_one_bit and restarted CABAC at every CTB row, even without
  entropy_coding_sync. It now does so only when that flag is set, and tiles are refused
  as unsupported. Before this, any multi-row non-WPP slice desynced at the row end.
- `ec-hw/src/dec/hevc.rs`: the hardware path fed flat IQ matrices whenever scaling was
  enabled. It now feeds the parsed lists (`iq_matrix`). Not run on hardware here.
- Struct-literal sites updated: ec-h265/src/encoder.rs, ec-hw/src/enc/headers.rs,
  ec-h265-syntax tests (fuzz.rs, slice.rs, ps.rs).
- `ec-h265/tests/scaling_lists.rs` (new): first output picture of each SLIST stream against
  ffmpeg, cropped to the window. `#[ignore]`d, see below.

## Verify (remote-run hevcscaling)
- `cargo clippy -p ec-h265 -p ec-h265-syntax --all-targets -- -D warnings`: clean.
- `cargo test -p ec-h265 -p ec-h265-syntax --release`: all pass. Includes the new unit tests
  in scaling.rs (default tables vs HM raster, round trips, pred-from-reference, DC copy,
  32x32 step 3, refusal of a bad delta, PPS-over-SPS precedence). One test ignored
  (scaling_lists, see below).
- `cargo test -p ec-h265 --release --test rdoq_cache_hash`: 3 passed.
- `cargo check --workspace --all-targets`: finishes. Warnings only in ec-av1 and ec-vorbis
  (not touched here).
- `cargo test -p ec-h265 --release --test dec_shell conformance_output_order`:
  142/147 PASS. Same five streams as before this lane (NoOutPrior_A, RAP_B, TSUNEQBD,
  VPSSPSPPS fail on output count or order, PS_B refused for an SPS extension).
  SLIST_A_Sony_5 65/65, SLIST_B_Sony_9 65/65, SLIST_C_Sony_4 65/65, str 65/65: PASS.

## Not done
- Pixel exactness of the SLIST streams. They are IDR plus B pictures (62 B, 3 I), and inter
  prediction is not implemented. The first picture is an IDR, but decoding it fails at its
  last CTB (CABAC desync, "end_of_slice_segment_flag 0 at CTB (12,7)"). The intra CTU loop
  has no SAO and no cu_qp_delta parsing. Not confirmed which one these streams use, but HM
  defaults to SAO. The test is `#[ignore]`d for this reason. Run with `--ignored` to see it.
- 32x32 chroma factors for 4:4:4 (matrixId 1, 2, 4, 5 derived from sizeId 2). Not needed
  for 4:2:0 and not coded here, so they hold the defaults.
- Hardware path (ec-hw) with real lists: not exercised on hardware.
