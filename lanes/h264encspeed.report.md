# lane h264encspeed: ec-h264 encode speed at Preset::Fast

Base: `d50d9476` (main). Worktree `~/.cache/wt/h264encspeed`, branch `lane/h264encspeed`.

## Goal and result

DONE.md item (ec-h264 encode P0): "1080p30 encodes at >= realtime with threads", with
`Preset::Fast`, at up to 6 threads.

| Criterion | Result |
|---|---|
| 1080p30 >= realtime (30 fps) with threads | **Not met.** 3.05 fps at 6 threads, 2.74 at 4, on host a (before: 1.48 and 1.70). About 10x short. |
| BD-rate vs x264 veryfast <= +20% (worst clip, `h264-1080p-23.976-8bit.mp4`) | **Met.** +10.57% (was +11.01%). |
| Encoder recon = ffmpeg decode, `scripts/h264-lane-check.sh recon` | **Met.** 28 MATCH, 0 MISMATCH. |
| Rate control within +-5% | **Met on 6/6**, 1080p 1 Mb/s is at +4.89% (was +4.79%). |
| `cargo test --release -p ec-h264` | **Pass** (see Verify). |
| `cargo clippy -p ec-h264 --all-targets -- -D warnings` | **Pass** (see Verify). |

The speed item is the only DONE.md criterion still open. The default Fast encode is about
2.2x faster on one thread and 1.6x faster at 4 threads than main, with no change to the
bitstream syntax and no loss in the BD-rate measure. The rest of the gap would need a
different encoder structure, which this lane did not attempt (see "What is left").

## The CPU-inflation question (suggested step 1)

The old report's "333 s at 1 thread, 546 s at 4 threads" was the sum of per-band wall
times, which includes time spent descheduled on a host shared with other lanes. The process
CPU time does not grow:

| threads | band wall sum (old metric) | process `cpu_user` |
|---|---|---|
| 1 | 127.9 s | 127.1 s |
| 4 | 233.5 s | 135.6 s |

(host a, 120 pictures, before the change; `EC_H264_SKIP_I8=0` run is the same bitstream as
main.) So the encoder does not inflate CPU work with threads: about 7% extra at 4 threads,
and that is the band seams. The wall-time slowdown is the host: the server is 4 vCPU and
load averages were 4 to 8 during every run, so 4 threads got about 2 cores. No per-macroblock
allocation was found to matter. The `EncEntropy` clone in `encode_mb` is 1% of time in the
`enc-profile` counters (`mb_timed!(5, ...)`), so I did not change it. No locks sit in the hot loops.
The atomics are debug counters only: the `enc-profile` timers and the `EC_H264_SHAPE_STATS`
counter in `choose_inter`, both off by default.

## Changes

All in `crates/ec-h264/src/enc/`. Nothing else changed. Both files are under 3,600 lines.

- `mb.rs`:
  - `FastKnobs` struct (new). Its defaults are the shipped Fast settings: `intra_gate: 2.0`,
    `skip_i8: false`. Balanced ignores all of it.
  - `IntraCost` now carries the Intra_16x16 SATD (`satd`). `InterChoice` carries the
    candidate's SATD (`satd`, the winner's value from the shape search, and the skip
    candidate's value).
  - `encode_mb`: when `Preset::Fast` and the best inter SATD is at most `intra_gate` times
    the Intra_16x16 SATD, the macroblock is coded inter only. The intra branch is skipped.
  - `encode_intra_mb`: Intra_8x8 is skipped when `skip_i8` is set. It is off by default.
- `mod.rs`: `fast_knobs()` reads the defaults and two sweep overrides:
  `EC_H264_INTRA_GATE=<number>` and `EC_H264_SKIP_I8=<anything but 0>`. It runs once per band.
- `lanes/bench.md` (new): speed rows and the variant sweep.

Settings I tried and rejected (numbers in `lanes/bench.md`):

- Intra_8x8 skipped. Fastest, and BD 10.08% with the gate. But
  `eight_by_eight_gains_on_flat_and_text` and `ffmpeg_decodes_bit_exactly` fail, since both
  assert that Intra_8x8 is coded and gains. Those tests are the quality contract, so I kept
  Intra_8x8 on.
- Gate ratios 0.5 and 1.0: the gate almost never fires on this content (speed unchanged).
- Motion sub-sample refinement one pass per step: 1% faster, the earlier lane's measured
  0.11 dB loss makes it not worth it. Not kept.
- Intra_4x4 with 1 RD candidate instead of 3: BD 10.92 with Intra_8x8 off. Not kept.
- No intra in P at all: BD 8.72% (better on the measure), 2x faster, but it has no path for a
  scene change inside a GOP and loses 0.19 dB of chroma PSNR. It is a bound, not a shipped
  setting.

## Verify

Each command ran through `scripts/remote-run.sh h264encspeed --host auto`. The speed table ran on
host a. One rate job fell back to the local machine and failed on a path, so it was re-run on
a host. The real-library test was run by hand on the local machine, since its source is there.

### Clippy and tests

- `cargo clippy -p ec-h264 --all-targets -- -D warnings`: exit 0, run again on the final code
  after the last doc-comment edit.
- `cargo test --release -p ec-h264 --no-fail-fast` (no `EC_REQUIRE_FIXTURES`): exit 0.
  Test binaries: 81 passed; 22 passed, 1 ignored; 15 passed, 1 ignored; 4 passed; 1 passed;
  11 passed; 1 passed. The 8x8 gain test and the bit-exact ffmpeg decode test pass.
- `real_library_frames_encode_and_decode_exactly` skips on the server: its source is listed in
  `fixtures/real-library-manifest.tsv` and lives in `~/Videos` on the user's machine only. With
  `EC_REQUIRE_FIXTURES=1` on the server it fails for that reason, not for a codec reason. Run
  locally (where the clip exists) with `EC_REQUIRE_FIXTURES=1`, it passes: 1 passed, 2.31 s.

### BD-rate (method from `lanes/h264encbd.report.md`)

`EC_REQUIRE_FIXTURES=1 EC_H264_X264_PRESET=veryfast
EC_H264_CLIP=fixtures/video/h264-1080p-23.976-8bit.mp4 cargo test --release -p ec-h264
--test encode bd_psnr_vs_x264 -- --nocapture`, converted with `scripts/bdrate.py --metric yuv`
(anchor x264, test ours).

| | BD-rate (%) | BD-PSNR-YUV (dB) |
|---|---|---|
| main | +11.01 | -1.008 |
| final | **+10.57** | -0.975 |

### Recon match

`scripts/h264-lane-check.sh recon` on host c: **28 MATCH, 0 MISMATCH**, exit 0. Covers 7
sizes (176x144 to 1920x1080, including macroblock-odd sizes) x 2 sources x CQP and ABR.

### Rate control (60 s, 30 fps, 4 threads, noisy testsrc2, `h264-lane-check.sh rate`)

| size | target (b/s) | achieved (b/s) | error | |
|---|---|---|---|---|
| 1280x720 | 1 000 000 | 1 016 008 | +1.60% | PASS |
| 1280x720 | 2 000 000 | 2 001 525 | +0.08% | PASS |
| 1280x720 | 4 000 000 | 3 999 602 | -0.01% | PASS |
| 1920x1080 | 1 000 000 | 1 048 884 | +4.89% | PASS (marginal) |
| 1920x1080 | 4 000 000 | 4 005 347 | +0.13% | PASS |
| 1920x1080 | 8 000 000 | 7 998 994 | -0.01% | PASS |

### Speed (1080p30, 4 s noisy testsrc2 = 120 pictures, 4 Mb/s, host a)

| threads | main fps | final fps | final realtime x |
|---|---|---|---|
| 1 | 0.94 | 2.07 | 0.069 |
| 2 | 1.52 | 2.68 | 0.089 |
| 4 | 1.70 | 2.74 | 0.091 |
| 6 | 1.48 | 3.05 | 0.102 |

CPU seconds (`cpu_user`) for the same runs: main 126 / 134 / 135 / 134; final 58 / 61 / 62 / 61.
Host load 4 to 8 on 4 vCPU. Rows are also in `lanes/bench.md`. The variant sweep
(per-variant CPU at 1 thread on hosts b and c) is in `lanes/bench.md` too.

## What is left

- **Speed (the open DONE item).** At 3 fps the 30 fps target is 10x away. The remaining
  per-picture cost at Fast is mostly `choose_inter` (motion search), the inter residual coding in
  `encode_inter_mb`, and Intra_4x4/8x8 on P macroblocks that the gate does not skip. The gate
  rarely fires on noisy content, so the noisy clip gained the most. Getting to 30 fps needs:
  a skip-and-early-exit structure for static macroblocks, a much smaller motion search with
  predicted starts, and SIMD SAD/SATD/DCT (suggested step 3). Step 3 was not attempted. A
  previous lane measured a transposed i32x4 SATD as flat, and SAD already uses i16x16.
  Motion search step 4 was not attempted beyond the sub-pel one-pass test (1%).
- **Host measurement.** Every run shared 4 vCPU with other lanes. The DONE item says "measured
  on the local 12-core machine" (D7). The local machine should be measured for the final
  number.
- The `enc-profile` counters and the `mb_timed!` wrappers are unchanged.
- `EC_H264_INTRA_GATE` and `EC_H264_SKIP_I8` are sweep overrides in production code, like the
  other `EC_H264_*` variables already in this crate.
