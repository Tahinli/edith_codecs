# Lane hevcperf: ec-h265 decode speed

Goal (DONE.md, "ec-h265 decode", speed bullet): 1080p30 at >= 2x realtime (<= 16.7 ms/frame)
and 4K30 at >= realtime (<= 33 ms/frame) on <= 6 threads, measured on the local 12-core machine.

Base: `main` at `8de08723`. Branch `lane/hevcperf`.

## Status

- 1080p: 17.0 ms/frame at 6 threads locally (target 16.7). Just over. Not met.
- 4K: 63.7 ms/frame at 6 threads locally (target 33). Not met.
- Conformance: still bit-exact (see verify). Checksums of decoder output are unchanged at every step.

## What changed

1. `crates/ec-h265/examples/decode_bench.rs`: decodes an Annex-B `.hevc` file, prints best-of-N
   ms/frame, realtime factor and an FNV checksum over every output plane. Timed runs exclude the
   checksum.
2. `scripts/gen-hevcperf-fixtures.sh`: libx265 streams (keyint=60, bframes=3, medium), written to
   `~/.cache/hevcperf-fixtures`, not committed. Three streams: `hevc1080_moving` (testsrc2 blended
   with mandelbrot plus grain), `hevc4k_moving` (same, 3840x2160), `hevc1080_test` (plain testsrc2).
3. SAO (`dec/sao.rs`): rewritten per CTB. Neighbour CTB availability (slice and tile rules) is
   computed once per CTB as a 3x3 table, and excluded (PCM/bypass) units are checked only for CTBs
   that contain them. The per-sample slice lookups are gone. Was 32% of single-thread time.
4. MC (`dec/mc.rs`): reference windows inside the plane are read in place, not gathered. Taps run in
   the outer loop so the multiply-add is over contiguous outputs. Edge-clamped windows are still
   gathered.
5. Inverse transform (`transform.rs`): the intermediate lives in `residual` (no 4 KB zeroed scratch per
   call). Columns and rows stop at the last non-zero input. Up to 16 non-zero inputs use a direct
   sum over the matrix rows, otherwise the existing butterfly.
6. Threads: `EC_H265_THREADS` (default `min(available_parallelism, 6)`, capped at 6;
   `dec::configured_threads`). The vertical deblock passes run on row bands (bands are multiples of
   8 luma rows, so no vertical edge spans two bands). SAO runs on CTB-row bands with a shared read-only
   input and disjoint output rows. The horizontal deblock pass is still serial: it reads and writes
   across rows, so it needs a snapshot-and-merge scheme (not done). Everything else is serial, so
   parallel decode is not reached.

Not done: WPP wavefront decoding (3b) and frame threading (3c). Both need the picture state to be
shared across threads, which in safe Rust means per-row locking or atomics on the planes and the
block-info maps. This was not attempted. Serial work per frame is most of the time now (about 14 ms
of 17 ms at 1080p), so threading the loop filters alone cannot reach the 4K target.

## Verify (exact commands, results)

- Conformance, after each step, on the server (`scripts/remote-run.sh hevcperf -- cargo test -p ec-h265 --release --test conformance_intra`):
  2 passed (conformance_pictures_match_the_sei_hash and whole_output_digest_reads_the_first_word).
  KNOWN_INEXACT is still empty, so 9203/9203 pictures stay exact.
- Full `cargo test -p ec-h265 -p ec-h265-syntax --release` on the server, final state (commit `c41f3f4e`):
  all binaries `ok`, 0 failed, 1 ignored (pre-existing). Run as `EXIT=0`.
- `cargo clippy -p ec-h265 -p ec-h265-syntax --all-targets -- -D warnings` on the server: `EXIT=0`, no warnings.
- Decoder output checksums (decode_bench) are identical at every step and thread count:
  1080p `2bfe18f415b7408d`, 4K `6d7084bb34f40906`.

## Bench (ms/frame, 120 frames, best of 3)

Local machine: `tFedora`, 12-core. Built and run with
`CARGO_TARGET_DIR=~/.cache/tgt-hevcperf TMPDIR=~/.cache/tmp-hevcperf cargo run --release -j4 -p ec-h265 --example decode_bench -- <file>`,
with `EC_H265_THREADS` set to 1, 2, 4 or 6. Other lanes were building on this machine during the
runs, so the numbers carry some noise.

| stream | 1 thread | 2 threads | 4 threads | 6 threads | target |
|---|---|---|---|---|---|
| 1080p moving | 18.68 (1.78x) | 17.41 (1.91x) | 17.09 (1.95x) | 17.04 (1.96x) | <= 16.7 |
| 4K moving | 73.47 (0.45x) | 67.72 (0.49x) | 65.75 (0.51x) | 63.69 (0.52x) | <= 33 |

server host `user@host-b` (4 vCPU, 7 GB), same binary, same fixtures:

| stream | 1 thread | 2 threads | 4 threads | 6 threads |
|---|---|---|---|---|
| 1080p moving | 40.34 | 42.38 | 41.88 | 40.96 |
| 4K moving | 156.71 | 155.02 | 167.98 | 178.83 |

The server has 4 vCPUs, so 6 threads are oversubscribed there. It is about 2.2 to 2.6 times slower than the
local machine, and threads gave no gain. The server numbers are not evidence for the target. The local
table is the one the goal is measured on.

Starting point for comparison: the same 1080p moving stream was 39.2 ms/frame single-threaded on the
local machine before this lane (the first harness run, with the checksum included in the timing).

## What is left

- 1080p is 0.3 ms over the 16.7 ms target at 6 threads. The next step is the serial path (MC
  about 25%, residual decode about 6%, weighted store about 5%, memmove about 7%, horizontal deblock
  about 3%) and the parallel horizontal deblock pass.
- 4K is about 2x over target. That needs WPP wavefront decoding, which means restructuring the CTU
  decode so it can run on several threads without `unsafe`, and/or frame threading.
- Per-TU allocations in `cu_syntax.rs` (`levels`, `scaled`, `samples`) are still heap allocations.
