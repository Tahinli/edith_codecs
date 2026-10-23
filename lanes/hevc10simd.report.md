# lane hevc10simd: ec-h265 Main 10 speed

## Result: criterion NOT met (realtime 1080p30 10-bit on 4 cores); small bit-identical gain only

Host: server a (4 vCPU, load average 0.5 to 1.9 at the start of each run, no other lane build seen).
Clip: synthetic `testsrc2` 1920x1080 with `scroll=v=0.02`, 30 frames, 8-bit and 10-bit (`yuv420p`, `yuv420p10le`),
`hevc_gop encode ... keyint 8, QP 30`, 4 threads, wall time of the whole process (includes yuv read and recon write).

| bit depth | fps before (3 runs) | fps after (3 runs) | output sha1 before = after |
|---|---|---|---|
| 8-bit | 17.06, 14.14, 16.72 | 18.54, 18.00, 18.86 | 29fc16181fe9 = 29fc16181fe9 |
| 10-bit | 12.90, 12.43, 12.78 | 13.64, 14.61, 14.52 | e8be339172d4 = e8be339172d4 |

10-bit goes from about 0.43x to about 0.48x realtime; 8-bit from about 0.55x to 0.62x. Gap 10-bit vs 8-bit stays about 22 to 25%.
Single thread, server a: 10-bit 6.9 s for 30 frames; 4 threads 2.4 s (2.9x scaling, so threading is not the limiter).

## Profile (perf, local Ryzen, 1 thread, 10-bit and 8-bit)

Nothing in the 8-bit path is hand SIMD: ec-h265 had no `std::arch` code; "8-bit is faster" is the same
generic code on `u8` ME planes and 16-bit `Pel` everywhere else, and ME already searches an 8-bit
copy (`y8`) for both depths. The profile is flat and near identical for both depths: ME SAD (`Search::cost`) 14%,
transforms (`forward_lines`, `transform_1d`, `inverse_1d`, `inverse_transform_bd`) about 18%, `code_tb` 6%,
`intra::predict` 5%, memmove 5%, `choose_luma_mode` SATD 5%, mc `filter_taps`/`half_band`/`weighted::store` about 8%, CABAC and residual about 8%.
The 10-bit extra cost is memory (857 MB vs 440 MB RSS) and wider coefficients/more residual work, not a missing kernel.

Experiment: building the whole crate with `-C target-cpu=x86-64-v3` (AVX2 autovectorisation of the
existing loops) gave no gain on server a (8-bit 16.8 to 19.1 fps, 10-bit 13.0 to 14.5 fps), so runtime AVX2 multiversioning of the
current loops is not worth adding; further gain needs restructured 16-bit-lane kernels, which is larger work than this lane.

## Changes (all output bit-identical)

- `enc_me.rs`: `sad_block` now has an SSE2 kernel (`_mm_sad_epu8`, `_mm_avg_epu8` for the rounded quarter-sample average), early exit every 4 rows; scalar twin kept for other targets. SSE2 is the x86_64 baseline, so no runtime detection is needed there. Test `sad_vector_path_matches_the_scalar_one`.
- `ctu.rs`: `satd` has an SSE2 16-bit-lane 4x4 Hadamard kernel (sums stay within +-16368 for up to 10-bit samples; used for n = 4 and multiples of 8, else scalar). Test `satd_vector_path_matches_the_scalar_one` (random and extreme inputs, 8 and 10-bit ranges).
- `lib.rs`: `forbid(unsafe_code)` became `deny(unsafe_code)` with local `#[allow(unsafe_code)]` on the two kernels (same pattern as ec-av1).

## Gates (server a)

- `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `EC_REQUIRE_FIXTURES=1 cargo test -p ec-h265 --release`: all pass, unchanged pins (main10, enc8_bitstream_hash, rdoq_cache_hash, conformance).
- 8-bit and 10-bit output hashes above equal before and after.

## Not done / next

Not done: u16 SIMD for interpolation, transforms/quant, intra prediction (profile shows no single kernel above 9%; AVX2 autovec showed no gain).
To reach realtime on 4 cores needs about 2.3x: options are 16-bit-lane (pmaddwd) transforms, a coarser RD search at 10-bit
(not bit-identical), or lowering the 10-bit frame buffers (RSS 2x). Bench scripts used were ad hoc (`benchdata/`, not committed).
