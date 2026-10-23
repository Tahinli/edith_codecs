# Lane hevcencthreads: deterministic multithreaded HEVC encode

Goal: a WPP-style wavefront in `crates/ec-h265` whose output is byte-identical at 1 to 6
threads, plus a measured speedup.

Base: `main` at `322837a2`, merged into the lane as `b738e256` (no conflicts). Branch
`lane/hevcencthreads`.

## Status

- Intra (I) pictures: threaded. `Encoder::code_picture_planes` runs one worker per CTB row
  (round-robin), with WPP context sync after CTB 1 of the row above.
- P pictures: threaded (`enc_p.rs`, from `b402db84`). `EncoderConfig::inter_wpp` keeps the
  sequential layout for comparison.
- Determinism test: `crates/ec-h265/tests/encode_threads.rs` now asserts identical access
  units and reconstruction at 1, 2, 4 and 6 threads for an intra-only stream (keyint 1) and an
  IDR+P stream (keyint 4). The P stream also decodes to the encoder reconstruction.
- Bench: `scripts/hevc-thread-bench.sh` times `hevc_gop` at 1, 2, 4 and 6 threads on 720p and
  1080p (testsrc2 with scroll, QP 30, GOP 8, 12 frames) and checks the output md5 per thread
  count.

## Commits

1. `b402db84` lane/hevcencthreads: P pictures as a wavefront, identical bytes at any thread count.
2. `b738e256` Merge main into lane/hevcencthreads.
3. This commit: 6-thread and intra-only identity tests, bench script, report.

## Bench (host b, `scripts/remote-run.sh hevcencthreads --host b -- scripts/hevc-thread-bench.sh`)

| clip | threads | secs (12 frames) | fps | speed-up vs 1 | bytes | identical |
|---|---|---|---|---|---|---|
| 1280x720 | 1 | 8.519 | 1.41 | 1.00x | 136772 | yes |
| 1280x720 | 2 | 4.972 | 2.41 | 1.71x | 136772 | yes |
| 1280x720 | 4 | 3.791 | 3.17 | 2.25x | 136772 | yes |
| 1280x720 | 6 | 3.646 | 3.29 | 2.33x | 136772 | yes |
| 1920x1080 | 1 | 15.529 | 0.77 | 1.00x | 269241 | yes |
| 1920x1080 | 2 | 8.608 | 1.39 | 1.80x | 269241 | yes |
| 1920x1080 | 4 | 6.663 | 1.80 | 2.33x | 269241 | yes |
| 1920x1080 | 6 | 6.475 | 1.85 | 2.40x | 269241 | yes |

Single timed run per point. Host b had a busy slot at start, so timings are not averaged.
The 6-thread gain over 4 is small: the default CTB is 32, so a picture has 23 CTB rows at 720p and
34 at 1080p, and the ramp-up and drain of the wavefront leave some workers idle.

## Verify (host a, `scripts/remote-run.sh hevcencthreads --host a -- ...`)

- `bash -c 'export EC_H265_REF_YUV_DIR=$HOME/refyuv/hevcinter; cargo test --release -p ec-h265 -p ec-h265-syntax -- --nocapture'`: exit 0, all suites pass.
  Conformance line: `total: 9203 pictures checked, 9203 exact, 147/147 streams accepted`.
  `encode_threads`: 3 passed (intra and IDR+P identity at 1/2/4/6 threads, decode round trip).
- `cargo clippy -p ec-h265 --all-targets -- -D warnings`: exit 0, clean.
- BD-rate: the multithreaded output is byte-identical to the 1-thread output at every tested
  thread count, so BD-rate against the single-thread encoder is unchanged by construction (delta 0).

## Left

- The bench is one run per point on a shared host; re-run with `RUNS=3` for stable numbers.
- `scripts/hevc-thread-bench.sh` only covers the default config (CTB 32, 8-frame GOP).
