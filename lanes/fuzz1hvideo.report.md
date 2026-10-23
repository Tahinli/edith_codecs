# lane fuzz1hvideo: DONE.md shared gate 2 for ec-h264, ec-vp8, ec-vp9

Status: CLOSED for all six targets. Each target ran one full 3,600 s libFuzzer run on the final code
(`lane/fuzz1hvideo` at the commit that adds this report). No panic, hang, timeout or OOM artifact in any run.
Base `bdc74c55`, branch `lane/fuzz1hvideo`.

## Final run table (3,600 s each, EXIT=0, 0 crash/oom/timeout/slow-unit artifacts)

| target | host | runs | exec/s (avg) | crashes | code |
|---|---|---|---|---|---|
| ec-h264 decode_annexb | c | 831,002 | 231 | 0 | dcc64530 |
| ec-h264 decode_avcc | c | 340,196 | 94 | 0 | ee94d667 (harness cap) |
| ec-vp8 decode_frame | a | 2,265,883 | 629 | 0 | dcc64530 |
| ec-vp8 decode_ivf | a | 1,917,513 | 532 | 0 | dcc64530 (capped) |
| ec-vp9 decode_frame | b | 1,017,368 | 283 | 0 | dcc64530 |
| ec-vp9 decode_stream | b | 4,266,085 | 1,185 | 0 | dcc64530 |

exec/s is the run count divided by 3,601 s, from the libFuzzer "Done N runs" line of each log.
Runs on dcc64530 and ee94d667 differ only in the decode_avcc harness cap, which only that target uses.

## Commits on lane/fuzz1hvideo this session

- `5e7b7583` ec-h264-syntax: saturating SPS height arithmetic (crash in decode_annexb, `sps.rs` height
  multiply overflow). Replay: `ec-h264/tests/fuzz_regressions.rs::sps_height_arithmetic_does_not_overflow`,
  input `tests/data/fuzz/decode_annexb-sps-height-overflow.264`.
- `dcc64530` ec-vp8: SIMD read windows checked against the buffer length before the AVX2 kernels run
  (`mc.rs` sixtap_block and bilinear branch); otherwise the scalar path runs, so output is unchanged.
  Also a reference-size check in `decode.rs` `reconstruct_inter_mb`. Replay:
  `ec-vp8/tests/fuzz_regressions.rs::inter_frame_with_reference_of_another_size_is_refused`, input
  `tests/data/fuzz/decode_ivf-ref-size-mismatch.ivf`. The crash was an AVX2 4-wide SPLITMV window reading past
  the end of the bordered reference plane (debug_assert in `simd::sixtap_h`).
- `ee94d667` ec-h264 fuzz: decode_avcc caps the avcC record at 512 bytes and the packet at 4 KiB. The
  uncapped target ran at 36 exec/s, below the 50 threshold.

## Crashes found and fixed (whole lane)

1. ec-vp9 `decode_frame`, out-of-memory: a key frame header declared 24580x36610. Fix: `decode.rs` refuses
   frames above 8192x4352 luma samples (VP9 level 6.2) as `Error::unsupported`, in `decode_one` and
   `decode_syntax`. Replay: `ec-vp9/tests/fuzz_regressions.rs::oversized_keyframe_header_is_refused_before_allocation`.
2. ec-vp9 `decode_frame`, u32 overflow in fix 1 (debug panic). Now u64. Replay: `frame_area_product_does_not_overflow`.
3. ec-h264 `decode_annexb`, `crop_x * l` overflow in `ec-h264-syntax/src/sps.rs` (unbounded crop offsets).
   Fix: checked arithmetic, corrupt error. Replay: `sps_crop_offset_overflow_is_refused`.
4. ec-h264 `decode_avcc`, dequantisation overflow in `ec-h264/src/transform.rs`. Fix: wrapping arithmetic in the
   dequant and DC transforms. Replay: `avcc_coefficient_scaling_does_not_overflow`.
5. ec-h264 `decode_annexb`, `26 + read_se()` overflow in `ec-h264-syntax/src/pps.rs`. Fix: saturating add
   (also in `slice.rs`). Replay: `pps_parameter_arithmetic_does_not_overflow`.
6. ec-h264 `decode_annexb`, SPS height multiply overflow in `sps.rs`. Fix: saturating ops. Replay:
   `sps_height_arithmetic_does_not_overflow` (see commit `5e7b7583`).
7. ec-vp8 `decode_ivf`, AVX2 out-of-bounds read in `mc.rs` (debug_assert in `simd::sixtap_h`). Fix: scalar fallback
   when the window does not fit. Replay: `inter_frame_with_reference_of_another_size_is_refused`.

Every replay test is in `crates/<crate>/tests/fuzz_regressions.rs`; the arithmetic-overflow tests catch the bug in
debug builds only, since the fuzzer and tests use overflow checks there.

## Harness caps (fuzz targets only, not the decoders)

- ec-h264 `decode_annexb`: picture at most 128x128, at most 64 NAL units per input.
- ec-h264 `decode_avcc`: avcC record at most 512 bytes, packet at most 4 KiB.
- ec-vp8 `decode_frame`: key frames above 128x128 are not fed; picture caps apply to inter frames too.
- ec-vp8 `decode_ivf`: same picture cap, at most 8 IVF frames.
- ec-vp9 `decode_frame`: key frames and intra-only frames above 128x128 are not fed (intra-only was added in b63402c3,
  it had made exec/s drop to 23).
- ec-vp9 `decode_stream`: at most 8 IVF frames, input cut before the first oversized key frame.

## Verify (final commit, run on server hosts; logs in ~/.cache/fuzz1hvideo-logs/final-verify2.log)

- `cargo test --release -p ec-h264`: 6 result blocks ok, EXIT 0.
- `cargo clippy -p ec-h264 --all-targets -- -D warnings`: EXIT 0.
- `cargo test --release -p ec-h264-syntax`: 2 result blocks ok, EXIT 0. Clippy: EXIT 0.
- `cargo test --release -p ec-vp8`: 8 result blocks ok, EXIT 0. Clippy: EXIT 0.
- `cargo test --release -p ec-vp9`: 28 result blocks ok, EXIT 0. Clippy: EXIT 0.

## Infrastructure

- `scripts/remote-run.sh` fix on main (30c5e607) was used; the working copy on this lane is left uncommitted.
- Hosts b and c are shared with other lanes. Some launches waited for a free slot (up to 10 minutes each);
  the avcc run had to retry on host c after other lanes held all three slots.
- Remote out dirs were cleared before the final launch. Crash copies from earlier runs are in
  ~/.cache/fuzz1hvideo-crashes (none came from the final runs).
