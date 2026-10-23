# lane hevcencspeed4: ec-h265 1080p30 P-GOP encode speed

Goal: 1080p30 8-bit IDR+P encode at >= 30 fps on <= 6 threads, measured locally.
Base: main dd323832 (hevcencspeed3: 6.90 fps at 6 threads, 2.50 at 1).

## Result

Not reached on the measurement runs I could get: best 28.60 fps (0.95x realtime) at 6 threads,
12.40 fps at 1 thread. The machine was never idle (load average 5-9 from other jobs on the 12
hardware threads, often 20-40), so every number is a lower bound. Single-thread cost fell 4.9x
(2.50 -> 12.40 fps) and 6 threads rose 4.1x (6.90 -> 28.60).

## Before / after (local, hevc-thread-bench, 30 frames, QP 30, GOP 8)

| threads | before fps | after fps | after realtime x |
|---|---|---|---|
| 1 | 2.50 | 12.40 | 0.41 |
| 2 | - | 21.35 | 0.71 |
| 3 | - | 25.47 | 0.85 |
| 6 | 6.90 | 28.60 | 0.95 |

Single-thread cycles on an 8-frame 1080p clip (perf stat): about 25.1G at the start, about 5.0G
at the end (instructions 15.1G -> 13.7G in the last tuning steps).

## Profile findings and changes

1. Motion estimation dominated (half-pel interpolation per candidate, dense +-16 full scan on
   every quadtree node). Rebuilt around padded 8-bit reference planes (whole, H, V, D half-pel,
   filled in parallel per picture), quarter-pel cost by averaging two planes, SAD with a per-row
   early exit. Full scan now strided (every 4th sample, range 24/12, gate 4 per sample) and a single
   sub-sample refinement pass.
2. Zero-residual shortcut in P `code_tb`: a block whose energy is below `24 * T^2` (T = smallest
   level-1 coefficient) skips the transform and is coded cbf=0.
3. Candidate rates: CABAC estimating mode for P candidates, and a closed-form residual rate
   (`estimate_residual_bits`) while estimating; AMVP candidates equal to a merge candidate are
   not costed; one intra trial instead of two.
4. Forward DCT computed across lines (SIMD-friendly butterfly, scratch sized per block); bit-exact
   against the old transform (unit test `fast_forward_matches_reference`).
5. P wavefront: per-row deblocking in the workers, bands copied straight into the final
   reconstruction and motion field; the serial whole-picture merge and deblock are gone.
6. Fast IDR (`gop_fast_idr`, default on when keyint > 1): no RDOQ, one RD candidate, no NxN, no
   transform skip, no RQT. This exposed a bug fixed here: P-picture intra units wrote
   `split_transform_flag` although the SPS then had `max_transform_hierarchy_depth_intra = 0`
   (stream failed to decode; caught by hevc-gop-roundtrip.sh and the encode_threads test).
7. Intra reference availability evaluated per 4x4 unit instead of per sample.

## BD-rate vs x265 veryfast (bframes=0, GOP 8, QP 22/27/32/37), YUV / Y, threads 2

| clip | BD-rate YUV | BD-rate Y |
|---|---|---|
| 352x288 testsrc2 | +13.0 | +9.6 |
| 1280x720 testsrc2 | +16.8 | +11.8 |
| 1280x720 natural | +18.8 | +15.7 |

All <= +25%. (hevcencspeed3 was about +12 at most; the spend of margin bought the speed.)

## Verify (server)

- `cargo clippy -p ec-h265 --all-targets -- -D warnings`, host b: exit 0.
- `scripts/hevc-gop-roundtrip.sh`, host b: all six rows ec decode exact and ffmpeg decode exact.
- `scripts/hevc-rc.sh` host b, 60 s 1280x720 testsrc2, keyint 30: 1000.1 / 2499.5 / 4999.2 kbps
  (+0.01 / -0.02 / -0.02 %), ec and ffmpeg decode match.
- Conformance, host a (`EC_H265_REF_YUV_DIR=$HOME/refyuv/hevcinter`): `total: 9203 pictures checked,
  9203 exact, 147/147 streams accepted`.
- `cargo test --release -p ec-h265`, host b: exit 0 (TESTRC=0), every test binary ok, 0 failed.
- No file over 5000 lines.

## What remains

- 30 fps at 6 threads needs about 5% more on the best run seen, and the runs were contended.
  Scaling is the weak spot: 1->2 threads gives 1.7x, 6 threads only 2.3x over 1 thread, partly
  from other jobs sharing the CPU, partly the per-picture barrier (wavefront ramp up/down every
  frame, plus serial pad/IO). Next steps: overlap picture N+1's first rows with picture N's tail
  (needs reference rows ready plus the motion range), GOP-level parallelism for constant QP, and
  cutting `Search::cost` per-call overhead (about 17% of cycles, ME is still the top item).
- Quality spend: BD is now up to +18.8%; the P defaults can be moved back toward quality
  (`EC_H265_*` knobs) once the speed target is met.
