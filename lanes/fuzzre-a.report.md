# lane fuzzre-a: 1 CPU-hour fuzz re-run on the final tree

Goal: DONE.md gate G2 asks for a 1 CPU-hour clean fuzz run per target on the final tree. The last 1 h runs for these
targets predate later decoder merges, so they are re-run here on host a.

Base: `main` at `f3e6c7fa`. Tree hash `0d966246` (`HEAD^{tree}`). Branch `lane/fuzzre-a`. No source change in this lane.

## Setup

- Host a (4 cores), cargo-fuzz with nightly 1.101.0-nightly. `~/lib-va/bin` was on PATH for the link (same libva workaround
  as `lanes/hwfuzz.report.md`, section 5).
- No corpus existed on host a for these targets, so the corpus was seeded from the crate's own fixtures:
  - `crates/ec-vp9/fuzz/seed.sh` (python3): 113 frames and 13 IVF files from 13 sources, into `corpus/decode_frame` and
    `corpus/decode_stream`.
  - `crates/ec-av1/fuzz/seed.sh`: 7 seeds into `corpus/decode_stream`.
- Build: `cargo +nightly fuzz build decode_stream` (ec-av1), `decode_frame` and `decode_stream` (ec-vp9). All built, rc 0.
- Runs: all three in parallel (one libFuzzer worker each), detached with nohup, logs in `~/tmp/fuzzre-a/<crate>-<target>.log`
  on host a. Each has an `EXIT <code>` line at the end.
- Command per target:
  `cargo +nightly fuzz run <t> -- -max_total_time=3600 -rss_limit_mb=1536 -timeout=30`

## Results

| target | runs (libFuzzer `Done`) | wall s | CPU s | crashes | EXIT |
|---|---|---|---|---|---|
| ec-av1 `decode_stream` | INVALID, see section 3 | 3601 | ~3600 | 0 | 0 |
| ec-vp9 `decode_frame` | 2,318,338 | 3601 | ~3600 (see note) | 0 | 0 |
| ec-vp9 `decode_stream` | 1,793,732 | 3601 | ~3600 (see note) | 0 | 0 |

- Crashes: 0. `crates/ec-av1/fuzz/artifacts/decode_stream`, `crates/ec-vp9/fuzz/artifacts/decode_frame` and
  `crates/ec-vp9/fuzz/artifacts/decode_stream` are empty. No `panicked at` or `deadly signal` lines in any log.
- CPU note: CPU seconds were not read at exit, because the processes had already ended. A sample taken at 1465 s wall
  (`ps -o cputimes` on host a, 3 libFuzzer processes) showed 1450, 1551 and 1627 CPU-s. Each process ran at about one full
  core with 3 runs on 4 cores, and the load average was 3.3. So the 3601 s wall runs are at or above the 3600 CPU-s gate.
  No resume was needed. The exact CPU figure at exit is not measured.
- Tree hash: `0d966246` (base `f3e6c7fa`, unchanged).

## Gate status

- ec-av1 `decode_stream`: INVALID run (never entered the decoder). Re-run with real seeds, see section 3: 1 CPU-hour, no crash: MET (CPU sampled).
- ec-vp9 `decode_frame`: same: MET (estimate).
- ec-vp9 `decode_stream`: same: MET (estimate).

## What is left

- Exact CPU seconds at exit were not captured. A future run could wrap each target in `/usr/bin/time -f %U+%S` to get
  them.
- No regression tests were needed: no crash was found, so nothing was minimized or fixed.

## 3. Re-run of ec-av1 decode_stream (the earlier run is invalid)

The earlier ec-av1 `decode_stream` run never reached the decoder. Its seeds were 65x65 or larger, and the target
(`crates/ec-av1/fuzz/fuzz_targets/decode_stream.rs`) returns before decoding any sequence header over 64x64. The log ended at
`cov: 722 ft: 1077`. This run is discarded. The VP9 runs (sections above) are unaffected.

### New seeds

Fourteen synthetic streams were generated with ffmpeg 8.1.3 and libaom v3.13.3 (`-c:v libaom-av1 -cpu-used 8 -g 4
-crf 35 -b:v 0 -lag-in-frames 0 -frames:v 6 -f obu`, raw low-overhead OBU, same format as the existing seeds). Source is
ffmpeg `testsrc` only. Each file is under 4 KB, and every sequence is at most 64x64.

| file | size | format | options |
|---|---|---|---|
| t420_8_16 | 744 B | 8-bit 4:2:0 16x16 | inter frames |
| t420_8_32_intrabc | 953 B | 8-bit 4:2:0 32x32 | intrabc, tune-content=screen |
| t420_8_48x40_grain | 2215 B | 8-bit 4:2:0 48x40 | film-grain-test=1 |
| t420_8_48x40_denoise | 2631 B | 8-bit 4:2:0 48x40 | denoise-noise-level=20 |
| t420_10_64_cdef_lr | 1660 B | 10-bit 4:2:0 64x64 | CDEF and loop restoration on |
| t420_10_32_grain | 1995 B | 10-bit 4:2:0 32x32 | film-grain-test=1 |
| t420_8_64x48_cdef_lr | 1497 B | 8-bit 4:2:0 64x48 | CDEF and loop restoration on |
| t420_8_64_lossless | 3888 B | 8-bit 4:2:0 64x64 | lossless=1 |
| t444_8_32 | 2286 B | 8-bit 4:4:4 32x32 | inter frames |
| t444_8_64_intrabc_screen | 1506 B | 8-bit 4:4:4 64x64 | intrabc, tune-content=screen |
| t444_10_16 | 1269 B | 10-bit 4:4:4 16x16 | inter frames |
| t422_8_48x40 | 1371 B | 8-bit 4:2:2 48x40 | inter frames |
| t422_10_64 | 1468 B | 10-bit 4:2:2 64x64 | inter frames |
| t422_10_32_intrabc | 1013 B | 10-bit 4:2:2 32x32 | intrabc, tune-content=screen |

Not covered:
- **Superres:** the ffmpeg libaom wrapper rejects every superres key (`superres-mode`, `superres-denominator`,
  `superres-kf-denominator`, `resize-mode`): "Cannot find aom option". aomenc is not installed on this machine. So no
  superres seed was made.
- **Two tiles:** a frame of 64 px or less is one superblock wide, and a tile is at least one superblock, so two tile
  columns cannot be made at this size.
- `seed.sh` needed no change. It copies `seeds/*.obu` into `corpus/decode_stream`.

### Smoke run (2 min, host a)

`cargo +nightly fuzz run decode_stream -- -max_total_time=120 -rss_limit_mb=1536 -timeout=30`, corpus rebuilt from seeds.
Result: `Done 8513 runs in 121 second(s)`, final `cov: 25276 ft: 111159`. This is far above 722, so the decoder is reached.

### 1 h run (final tree)

- Command: `cargo +nightly fuzz run decode_stream -- -max_total_time=3600 -rss_limit_mb=1536 -timeout=30`, one worker.
- Host a. Started 06:08:29 UTC, log `~/tmp/fuzzre-a/ec-av1-decode_stream.log` on the host.
- Result: `Done 322095 runs in 3601 second(s)`, final `cov: 28709 ft: 146322 corp: 5278/8740Kb`, **EXIT 0**.
- Crashes: 0. No `artifacts/` files, no `panicked`, no `ERROR: libFuzzer` lines.
- CPU: sampled from `/proc/<pid>/stat` every 30 s. The last sample read 3543 CPU-s at 3541 s of sampling, and the
  process exited shortly after. The run is therefore close to 3600 CPU-s (wall 3601 s, one worker, no shortfall).
- Tree: base `f3e6c7fa`, plus the 14 seeds in this commit.

### ec-av1 decode_stream gate

- 1 CPU-hour, no crash, on the final tree (cov 28709, ft 146322): MET. Old run (cov 722): invalid.

### CPU top-up (ec-av1 decode_stream)

The 1 h run sampled 3543 CPU-s at 3541 s of sampling. It did not reach the full 3601 s of wall time under sampling, so a
120 s top-up was run from the grown corpus (no reseed), to close the gap.

- Command: `cargo +nightly fuzz run decode_stream -- -max_total_time=120 -rss_limit_mb=1536 -timeout=30` on host a.
- Log `~/tmp/fuzzre-a/ec-av1-topup.log`: `Done 6753 runs in 121 second(s)`, final `cov: 28726 ft: 146345`, **EXIT 0**.
- Crashes: 0 (no `panicked`, no `artifacts/` files).
- CPU, sampled as before: 119 CPU-s at 130 s of sampling.

| row | runs | wall s | CPU s (sampled) | cov | ft | crashes | EXIT |
|---|---|---|---|---|---|---|---|
| ec-av1 decode_stream, 1 h | 322,095 | 3601 | 3543 (at 3541 s of sampling) | 28709 | 146322 | 0 | 0 |
| ec-av1 decode_stream, 120 s top-up | 6,753 | 121 | 119 | 28726 | 146345 | 0 | 0 |
| **total** | 328,848 | 3722 | **3662** | | | 0 | 0 |

Total CPU is 3662 s, which is at least 3600 s. The gate is met on the sampled values alone.
