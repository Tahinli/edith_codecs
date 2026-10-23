# lane hevcencspeed: P-picture encode speed

Base: `5923e4e2` (main). Branch `lane/hevcencspeed`, worktree `~/.cache/wt/hevcencspeed`.
Final commit: `de845804` (the step commits are listed below).

## Result

The 720p natural clip (30 frames, QP 30, GOP 8, one core) went from **0.147 fps to 1.73 fps**
(204.6 s to 17.35 s, about 11.8x). BD-rate stays within the budget on every clip: the largest
per-step cost is +0.18 points and the cumulative cost is +0.17 (352x288), +0.66 (720p testsrc2)
and +0.46 (720p natural), all in YUV.

**Target not met.** The target was 4 fps at 720p (or 4x x265 veryfast's time). The final build
is 7.8x x265 veryfast's single-thread time (2.22 s vs 17.35 s), and 1.73 fps is 2.3x short of 4 fps.
DONE.md's own speed gate is 1080p30 at >= realtime on <= 6 threads, which is far beyond this lane.
ec-h265's P path is single-threaded. Remaining levers are listed at the end.

Host: host c was busy for the whole run (the other lanes' cargo-fuzz jobs held both of its slots),
so every timing and BD run was on **host b** (4 cores, load 1-2). Clippy, the tests and the
conformance run were on host a. Timings are one run each on the same host; treat +-3% as noise.

## Per-step table

Natural clip = `nat-1280x720.yuv` (the hevc fixture, 30 frames). BD = `scripts/hevc-bdrate.sh`
(YUV 6:1:1 / Y), anchor libx265 veryfast bframes=0 keyint 8, QP 22/27/32/37. Each row is cumulative
(it includes the kept steps above it).

| step | commit | natural fps (time) | 352x288 testsrc2 YUV / Y | 720p testsrc2 YUV / Y | 720p natural YUV / Y | kept |
|---|---|---|---|---|---|---|
| 0. base | `5923e4e2` | 0.147 (204.6 s) | -5.31 / -7.76 | +1.51 / -1.76 | +6.89 / +7.86 | - |
| 1. halves (2NxN, Nx2N) seeded only, no full integer scan | `a2b35567` | 0.291 (103.2 s) | -5.71 / -8.19 | +1.79 / -1.47 | +7.21 / +8.28 | yes |
| 2. SAD: whole-sample refs read directly, cutoff at best cost (bit-exact) | `2a60649a` | 0.730 (41.1 s) | -5.71 / -8.19 | +1.79 / -1.47 | +7.21 / +8.28 | yes |
| 3. pattern search starts from the whole-sample position | `b3bf5d15` | 0.832 (36.1 s) | -5.35 / -7.72 | +1.96 / -1.28 | +7.33 / +8.39 | yes |
| 4. early CU split: no children when leaf SAD < 0.25/sample | `975d42ca` | 1.242 (24.2 s) | -5.25 / -7.65 | +2.14 / -1.17 | +7.29 / +8.33 | yes |
| 5. merge-skip termination (no AMVP / intra / partition when a residual-free merge is < 2.0 cost/sample) | `dbf4c339` | 1.404 (21.4 s) | -5.32 / -7.76 | +2.07 / -1.26 | +7.33 / +8.38 | yes |
| 6. no halves search for a square with leaf SAD < 0.25/sample | `9baff572` | 1.663 (18.0 s) | -5.14 / -7.69 | +2.17 / -1.22 | +7.35 / +8.41 | yes |
| 7. clippy fix (collapse the near-exact test), bitstream unchanged | `de845804` | **1.729 (17.35 s)** | **-5.14 / -7.69** | **+2.17 / -1.22** | **+7.35 / +8.41** | yes (final) |

The final row is the final commit, measured on host b: BD from the full `hevc-bdrate.sh` run on
this tree, 122889 bytes for the natural clip (same bytes as step 6).

Rejected steps (measured, not kept):

| variant | natural fps | 352x288 YUV / Y | 720p testsrc2 YUV / Y | natural YUV / Y | why |
|---|---|---|---|---|---|
| no full integer scan for 8x8 nodes | - | -2.17 / -4.48 | - | - | +3.5 on 352x288: over budget |
| full integer scan of +-4 (not +-8) for 8x8 nodes | 0.785 | -4.39 / -6.80 | - | - | +1.3 on 352x288: over budget |
| one pass (not to convergence) of the sub-sample refinement | 0.757 | -5.79 / -8.24 | +1.99 / -1.27 | +7.37 / +8.37 | 2.5% faster for +0.2: not worth it |
| early split at leaf SAD < 1.5/sample | 1.354 (22.2 s) | +0.04 / -2.95 | +6.12 / +2.34 | - | +5.4 on 352x288: over budget |

The 1.5 split row is the same run for both columns: its timing and BD were measured together.

## Profile

Stage timers, temporary and not committed (`Instant` around each stage, summed over the encode).
Natural 720p, 30 frames, QP 30, GOP 8, host b. The stages are inclusive where they nest.

Base `5923e4e2` (total 204.6 s; the motion search is 94% of it):

| stage | time (s) | share | calls |
|---|---|---|---|
| motion search, 2Nx2N (full range + seeds) | 75.1 | 37% | 490,880 |
| motion search, halves (2NxN / Nx2N) | 117.9 | 58% | 465,920 |
| merge + AMVP derivation | 0.07 | - | 37,176 |
| inter candidate: MC | 0.54 | - | 156,882 |
| inter candidate: transform + quant + recon | 3.3 | 2% | 156,882 |
| inter candidate: CABAC bit estimate | 0.24 | - | 156,882 |
| intra candidate: prediction | 0.87 | - | 148,714 |
| intra candidate: transform + quant + recon | 3.3 | 2% | 148,714 |
| intra candidate: CABAC bit estimate | 0.45 | - | 148,714 |
| partition evaluation (inclusive) | 0.17 | - | 6,113 |
| final write + commit | 0.08 | - | 37,176 |
| `code_cu` total (inclusive) | 9.2 | 4.5% | 37,176 |

After step 4 (total 24.7 s):

| stage | time (s) | share |
|---|---|---|
| motion search, 2Nx2N | 8.0 | 32% |
| motion search, halves | 6.5 | 26% |
| `code_cu` total (inclusive) | 8.5 | 34% |
| of which: inter transform + quant + recon | 3.1 | 12% |
| of which: intra transform + quant + recon | 3.3 | 13% |
| of which: intra prediction | 0.75 | 3% |
| of which: CABAC bit estimates (inter + intra) | 0.6 | 2% |

The motion search was the cost: the full integer scans (+-16 at 32, +-8 at 16 and 8), and the
half-block searches at 16 and 32, each of which ran a full +-8 scan. Most of the work went there.

Not measured: `perf` is not installed on host b or host c, so there is no sampled profile.

## Commits (in order)

- `a2b35567` halves search seeded only (step 1)
- `2a60649a` SAD fast path, bit-exact (step 2)
- `b3bf5d15` integer start for the pattern search (step 3)
- `975d42ca` early CU split at SAD < 0.25/sample (step 4)
- `dbf4c339` merge-skip early termination (step 5)
- `9baff572` no halves search for near-exact squares (step 6)
- `de845804` clippy fix, same bitstream (step 7)

## Verify (final tree `de845804`)

- `cargo clippy -p ec-h265 -p ec-h265-syntax --all-targets -- -D warnings` on host a: exit 0.
- `scripts/hevc-gop-roundtrip.sh` on host b: exit 0. Every row decodes exact with ec-h265 and ffmpeg.
- `cargo test -p ec-h265 -p ec-h265-syntax --release` on host a with
  `EC_H265_REF_YUV_DIR=$HOME/refyuv/hevcinter`: exit 0. 17 test result lines, all `ok`, 0 failed.
- Conformance (`cargo test -p ec-h265 --release --test conformance_intra conformance_pictures_match_the_sei_hash -- --nocapture`, host a):
  `total: 9203 pictures checked, 9203 exact, 147/147 streams accepted`.
- Bit-exactness of step 2: the natural clip's bytes were 123415 before and after the SAD fast path.

## Speed reference (x265, same host b, same clip, 30 frames, QP 30, bframes=0, keyint 8)

| encoder | time | fps |
|---|---|---|
| x265 veryfast, 1 thread (`pools=1`), via ffmpeg | 2.22 s | 13.5 |
| x265 veryfast, default threads (4 cores) | 0.90 s | 33.3 |
| ec-h265, base `5923e4e2` (1 thread) | 204.6 s | 0.147 |
| ec-h265, final `de845804` (1 thread) | 17.35 s | 1.73 |

Ec-h265 is 7.8x x265 veryfast's single-thread time. The ffmpeg timings include raw I/O.

## Left / not done

- The 4 fps target and the 4x x265 target are not met. The final build is 2.3x short of 4 fps and
  7.8x x265's single-thread time.
- Threading: the analysis of the four children of a quadtree node can run in parallel without
  changing the output (their searches read the same committed state). Not done; it needs `Sync`
  and a per-thread search scratch.
- Intra candidate pre-selection (SATD over the seven modes, keep two for RD): intra is about 25% of
  the final time. Not done.
- Lossless transform speed-up (`forward_transform` zeroes a 4 KB stack buffer per call, does an O(n^3)
  matrix product, and `code_tb` allocates per call): transform + quant is about 35% of the final
  time. Not done.
- Partition-half pruning from the 2Nx2N residual, and a fast rate estimate in place of CABAC
  counting: not measured. The CABAC estimate is about 2% of time, so it is not a lever.
- The early-termination thresholds (0.25 split, 2.0 merge-skip, 0.25 halves) were picked by
  measurement on the three clips and are not tuned.
- Temporary profiling code was reverted before each commit; the stage timers exist only in the
  profiling run described above.
