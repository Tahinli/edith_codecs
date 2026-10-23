# lane/hwgaps: four ec-hw / ec-va gaps from edith's engine-hw

Base: `main` at `d6eb322c`. Branch `lane/hwgaps`. Local GPU (radeonsi, `/dev/dri/renderD128`), `-j4`,
`CARGO_TARGET_DIR=~/.cache/tgt-hwgaps`, `TMPDIR=~/.cache/tmp-hwgaps`.

## Commits
1. `3204d0cc` H.264 encode profile (Constrained Baseline, Main, High).
2. `00d477bb` CBR rate control: HRD starts full; measurement test.
3. `0b6a8ccc` DMA-BUF import in ec-va; `Encoder::encode_surface` in ec-hw.
4. Broken-GOP refusal (AV1, HEVC, VP9 check; H.264 already refused) with `a_broken_gop_is_refused_on_every_codec`.

## 1. H.264 encode profile
- `ec_hw::H264Profile { ConstrainedBaseline, Main, High }` (default High, the previous behaviour).
  `EncoderConfig::h264_profile` field and `EncoderConfig::h264_profile(self, H264Profile) -> EncoderConfig`.
- Main: `profile_idc` 77, no High SPS tail (chroma/bit depth/scaling). Constrained Baseline: `profile_idc` 66,
  `constraint_set1_flag`, CAVLC (`entropy_coding_mode_flag` 0). No 8x8 transform is written in any profile
  (the PPS has no extension, `transform_8x8_mode_flag` stays 0).
- Tests (GPU, `--include-ignored` not needed: the crate's GPU tests skip without a display):
  `h264_constrained_baseline_encodes_and_decodes`, `h264_main_profile_has_no_high_syntax_and_decodes`,
  `h264_high_profile_still_decodes`: ffprobe reports `Constrained Baseline` / `Main` / `High`, ffmpeg decodes with
  empty stderr, worst PSNR 47.8 dB (Main, Baseline) vs source.
- Pure: `enc::headers::h264_profile_tests` (profile_idc and constraint bytes; Main SPS is 7-8 bits shorter than High).

## 2. CBR rate control
Measured with `cbr_lands_within_five_percent_of_the_request` (GPU). 1280x720 at 30 fps, 240 frames (8 s),
GOP 30, `ConstantBitrate`, default `EncoderConfig`. Two clips: a synthetic moving box, and ffmpeg `testsrc2`.

| codec | clip | request | delivered | error | PSNR |
|---|---|---|---|---|---|
| H.264 | moving box | 1000 kb/s | 1000 kb/s | +0.0% | 64.54 dB |
| H.264 | moving box | 3000 kb/s | 3000 kb/s | +0.0% | 70.87 dB |
| H.264 | moving box | 8000 kb/s | 8000 kb/s | +0.0% | 71.50 dB |
| H.264 | testsrc2 | 1000 kb/s | 1010 kb/s | +1.0% | 37.48 dB |
| H.264 | testsrc2 | 3000 kb/s | 3033 kb/s | +1.1% | 44.04 dB |
| H.264 | testsrc2 | 8000 kb/s | 8166 kb/s | +2.1% | 57.23 dB |
| HEVC | moving box | 1000 kb/s | 1001 kb/s | +0.1% | 65.33 dB |
| HEVC | moving box | 3000 kb/s | 3001 kb/s | +0.0% | 71.42 dB |
| HEVC | moving box | 8000 kb/s | 8001 kb/s | +0.0% | 71.00 dB |
| HEVC | testsrc2 | 1000 kb/s | 1011 kb/s | +1.1% | 38.00 dB |
| HEVC | testsrc2 | 3000 kb/s | 3026 kb/s | +0.9% | 43.34 dB |
| HEVC | testsrc2 | 8000 kb/s | 8094 kb/s | +1.2% | 54.99 dB |
| AV1 | moving box | 1000 kb/s | 1001 kb/s | +0.1% | 61.05 dB |
| AV1 | moving box | 3000 kb/s | 3001 kb/s | +0.0% | 70.01 dB |
| AV1 | moving box | 8000 kb/s | 8001 kb/s | +0.0% | 70.13 dB |
| AV1 | testsrc2 | 1000 kb/s | 1010 kb/s | +1.0% | 36.60 dB |
| AV1 | testsrc2 | 3000 kb/s | 3034 kb/s | +1.1% | 42.58 dB |
| AV1 | testsrc2 | 8000 kb/s | 8007 kb/s | +0.1% | 57.56 dB |

Before the fix (same table, HRD starts at 3/4 full): moving box -12.4% to -12.5% on all three codecs; testsrc2
H.264 +15.4% / +1.9% / -5.0%, HEVC +14.2% / +2.2% / -6.6%, AV1 +2.1% / -2.6% / -7.8% (all at 240 frames).

Cause: the HRD buffer (one rate window) started three quarters full, so the driver's budget was short by a quarter
of a window, i.e. 0.25 s. Over a 2 s clip that is 12.5%, exactly the measured undershoot; over 8 s it is 3%.
Fix: `initial_buffer_fullness = buffer_size`. Removing the HRD buffer entirely also gives exact moving-box rates,
but the start-full buffer keeps the driver's per-picture budget.

Not the cause (measured): the rate-control window (1000 vs 1500 ms), `rc_flags` frame-skip bit, and the QP range
min/max/initial (cros-like 0..51, initial 25): none changed the output for H.264 or HEVC.

Caveat: a 60-frame (2 s) testsrc2 run still overshoots by +14% to +16% at 1 Mb/s for H.264/HEVC (start-up of
the controller); the 240-frame measurement is the one the +/-5% gate uses. The gate test uses 240 frames.

Not measured in this lane: PSNR against cros-codecs at the same rate. That needs the edith build with the vendored
cros encoder, which this lane does not carry. The 0.72 dB figure in `hwfixes`/`enginehw` was at 512x384, 4 Mb/s,
and is not comparable with the table above.

## 3. DMA-BUF import (ec-va)
- `ec_va::Surface::import_prime(display: &Arc<Display>, prime: PrimeSurface) -> Result<Arc<Surface>>`
  (`ec-va/src/surface.rs`). NV12 and P010 only. Uses `VASurfaceAttribMemoryType = VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME_2`
  and `VASurfaceAttribExternalBufferDescriptor` (new `sys::VASurfaceAttribExternalBufferDescriptor = 7`) pointing at a
  `VADRMPRIMESurfaceDescriptor` built from the `PrimeSurface` (objects, layers, modifiers as given).
  The fds are held by the surface until it drops.
- The legacy `VASurfaceAttribExternalBuffers` (MEM_TYPE_DRM_PRIME) path is not implemented: radeonsi accepts PRIME_2
  import, so it is not needed here.
- Test `dma_buf_import_reads_back_identical_pixels` (ec-va GPU): paint NV12, export, import, read both back: planes
  identical and non-zero; fd count returns to baseline after drop.
- `ec_hw::Encoder::encode_surface(&mut self, surface: &Arc<ec_va::Surface>, display_size: (u32, u32), meta: FrameMetadata) -> Result<CodedFrame>`.
  The previous private `encode_surface` is renamed `encode_settled`. A surface of another size or depth goes through
  the VPP conversion.
- Test `encode_surface_of_an_imported_dma_buf_matches_the_cpu_path` (ec-hw GPU): H.264 and HEVC, 4 frames, the
  imported-DMA-BUF encode is byte-identical to `Encoder::encode` on the same pixels.

## 4. Broken GOP refusal
- AV1 (`ec-hw/src/dec/av1.rs` `decode_frame`): an Inter or Switch frame whose `ref_frame_idx[0..7]` names a slot no
  decoded frame filled is refused with `Error::Stream(corrupt(...))`. Before: the fixture's inter frame decoded to a
  picture. The spec requires RefValid for all seven references; libaom refuses the same case.
- HEVC (`ec-hw/src/dec/hevc.rs`): `apply_rps` returns `Result`. A reference picture set entry marked used for
  prediction that is not in the DPB is refused; so is a P/B slice whose reference list is empty. Before: the broken GOP
  decoded without error and emitted nothing, with `0xff` ref indices in the parameters.
- VP9 (`ec-hw/src/dec/vp9.rs`): an Inter frame naming an empty slot in LAST/GOLDEN/ALTREF is refused. The VP9 syntax
  parser already refused the test fixture first (`frame_size_with_refs names empty reference slot`); the ec-hw check
  covers an inter frame that codes an explicit size.
- H.264: already refused (`H.264 inter slice with an empty reference list`).
- Test `a_broken_gop_is_refused_on_every_codec` (ec-hw GPU): AV1 `av1-1080p-23.976-8bit.ivf` and VP9
  `vp9-1080p-23.976-8bit.ivf` (first temporal unit dropped; AV1 keeps the sequence header OBU), H.264
  `h264-bframes-320.264` and HEVC `hevc-bframes-320.265` (parameter sets of the first access unit kept). Each must return
  an error and output zero pictures. The edith fixture `test_av1.mkv` is not on this box; the fixture used is the
  equivalent constructed from the local IVF.

## New API
- `ec_hw::H264Profile` (re-exported), `EncoderConfig::h264_profile` (field), `EncoderConfig::h264_profile(H264Profile)`.
- `ec_hw::Encoder::encode_surface(&mut self, &Arc<ec_va::Surface>, (u32, u32), FrameMetadata) -> Result<CodedFrame>`.
- `ec_va::Surface::import_prime(&Arc<Display>, PrimeSurface) -> Result<Arc<Surface>>`.
- `ec_va::sys::VASurfaceAttribExternalBufferDescriptor: VASurfaceAttribType = 7`.

## Gates
- `cargo clippy --no-deps -p ec-hw -p ec-va --lib --tests -- -D warnings`: exit 0 (after fixing 8 pre-existing lints in
  ec-hw's tests and `dpb264.rs`; earlier this command failed on those, not on ec-av1).
- `cargo test --release -p ec-hw --lib`: 17 passed. `-p ec-va --lib`: pass.
- `cargo test --release -p ec-hw --test gpu -- --skip av1 --skip real_library`: 32 passed plus the new broken-GOP test.
- `cargo test --release -p ec-va --test gpu`: 14 passed (13 earlier plus the import test) with `--test-threads=1`.
- AV1 decode-vs-ffmpeg tests `av1_matches_ffmpeg`, `av1_10bit_matches_ffmpeg`: pass (2/2).
- SKIPPED for time (ffmpeg with librav1e, over 10 minutes each, per the lane rule): `av1_encode_agrees_between_two_decoders`,
  `av1_10bit_encode_agrees_between_two_decoders`, `av1_encode_bd_row_vs_software`, `av1_10bit_bd_row_vs_software`,
  `av1_encode_rate_control_lands`, `av1_encode_speed`. Also not run: `real_library_spot_check` (library absent here),
  `long_decode_does_not_grow_the_resident_set`. The AV1 HW encoder path is unchanged by this lane except the HRD fullness
  (measured by `cbr_lands_within_five_percent_of_the_request`, which includes AV1 and passes).

## Left
- Legacy `VASurfaceAttribExternalBuffers` import path (not needed on radeonsi).
- PSNR against cros-codecs at a matched rate (needs the edith build).
- VP9 broken-GOP fixture with an explicit-size inter frame (only the parser refusal is covered).
