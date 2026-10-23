# lane-av1threads: 1080p30 decode threading, default thread count

Result: **the 1080p30 realtime criterion (DONE.md, ec-av1 decode P0) is NOT met.** The
default thread count did change to `min(available_parallelism, 6)`. Frame workers alone do
not reach 30 fps on this stream. The only knob that does is the recon wavefront
(`EC_AV1_RECON_THREADS`), and it is not bit-exact on every stream, so it was not made the
default. Blocker below.

Base: `main` at `db540eba`. Worktree `~/.cache/wt/av1threads`, branch `lane/av1threads`.

## Changes

- `crates/ec-av1/src/autothreads.rs` (new, 91 lines): `default_threads()` =
  `min(available_parallelism, 6)`; `decode_threads_from(EC_AV1_THREADS)`. A positive value
  wins (so `EC_AV1_THREADS=1` forces one thread). Unset or invalid gives the default. Under
  `cfg(test)` the unset default is 1 (see below). Three unit tests; the
  one that matters is `frame_workers_decode_the_same_pictures_as_one_thread`, which decodes two 4:2:0 fixtures at
  1 and 4 frame workers and compares every shown picture's planes.
- `crates/ec-av1/src/stream/mod.rs`: `decode_threads()` now calls `autothreads` (was: unset = 1).
- `crates/ec-av1/src/lib.rs`: `mod autothreads;`.
- `crates/ec-av1/src/stack_budget.rs`: `autothreads.rs` added to the by-value scan list
  (the scan fails on any module it does not read).
- `crates/ec-av1/examples/decode_bench.rs` (new): decodes an OBU stream, prints
  `threads=… frames=… fps=…`. `--threads N` calls `decode_stream_with_threads` directly;
  without it the decoder default (env) applies. `--raw` writes 8-bit planar YUV to stdout, for
  an md5 comparison with ffmpeg.
- `lanes/bench.md` (new): the bench rows.

`par.rs`, `decode/`, `encode/` and `encoder/` are untouched. Largest file touched:
`stream/mod.rs` at 2333 lines.

### Why `cfg(test)` pins the unset default to 1

With the default at min(cores, 6) and the test binary left alone, 151 gates failed in one
run. Their messages are "never fired", "never observed", "zero hits". The gates read
thread-local counters that frame workers never increment. The crate's own comments say so
("the default is 1" for this reason). No pixel assertion failed in that run. The test build
therefore keeps the single-threaded default, and the threaded path is pinned by the explicit
gate above. Product builds get `min(cores, 6)`.

## Stream and fixture

`ffmpeg -f lavfi -i testsrc2=size=1920x1080:rate=30 -t 10 -pix_fmt yuv420p -c:v libaom-av1 -cpu-used 8 -row-mt 1 -tiles 2x2 -b:v 4M -f obu t1080.obu`
on host a, at `~/av1threads-bench/t1080.obu` (7,120,251 bytes, 300 frames, 622.08 Mpx).

## Timing (host a, 4 cores, `EC_AV1_THREADS=n`, median of 3, load 3 to 7)

| threads | fps | realtime x |
|---|---|---|
| 1 | 21.0 | 0.70x |
| 2 | 20.5 | 0.68x |
| 4 | 20.7 | 0.69x |
| 6 | 18.8 | 0.63x |
| default (= min(4,6) = 4 on this host) | 20.6 | 0.69x |

Frame workers do not help here. The stream has a reference-heavy GOP, so few frames are
leaves. Six threads on a 4-core host is oversubscribed and slower than one.

Recon knob, unchanged main code, single runs, same host:

| config | fps | realtime x |
|---|---|---|
| 6 frame threads + `EC_AV1_RECON_THREADS=4` | 32.5 | 1.08x |
| 6 frame threads + `EC_AV1_RECON_THREADS=2` + `EC_AV1_FILTER_THREADS=4` | 34.5 | 1.15x |

## Bit-exactness

- Output md5 (`decode_bench --raw`) equals `ffmpeg -f rawvideo -pix_fmt yuv420p` on the 300
  frames: `3adc24bb024913aab921dc8575b8be8b`, at `EC_AV1_THREADS` = 1, 4 and 6.
- **Blocker, recon wavefront:** `EC_AV1_RECON_THREADS=4` (with frame workers at 1 or 4)
  fails `stream::tests::t01::a_real_aomenc_quantisation_matrix_stream_decodes_pixel_exact`:
  "qm-off control frame 1 plane 0 differs from ffmpeg (got 33, want 32)". The same test
  passes with `EC_AV1_RECON_THREADS=1`. The same stream passes with `EC_AV1_FILTER_THREADS=4`.
  This is the same class as the `DEFECT B` note in `lanes/av1lrflush.report.md`. The
  `av1wavefront` fix covered the 4:4:4 subsampling default only. Turning recon threads on by
  default would therefore make edith's decode wrong on some streams. Not done.
- The first attempt at scoping the recon and filter knobs to the default count produced
  about 185 gate failures, including the pixel mismatch above and a pyramid-stream mismatch
  ("ours 88 vs ffmpeg 87"). It was reverted.

## Verify

| command | where | result |
|---|---|---|
| `cargo build --release -p ec-av1 --example decode_bench` | host a | ok |
| decode timing table above (`EC_AV1_THREADS=n decode_bench t1080.obu`) | host a | as tabled |
| md5 vs ffmpeg at 1, 4, 6 threads | host a | all `3adc24bb…`, equal to ffmpeg |
| `cargo clippy -p ec-av1 --all-targets -- -D warnings` | host c | lib: 15 errors, lib test: 39. All in `encode/` and `encoder/`. None elsewhere (same 39 as the av1clippy2 baseline) |
| `EC_MEMGUARD_MAX=6G cargo test --release -p ec-av1 -- --test-threads=4` | host c | **810 passed / 0 failed / 49 ignored**, GATE_RC=0 (807 before, plus the 3 new autothreads tests) |

Host b was busy during the first clippy attempt (slot held by another lane), so both checks
ran on host c.

## Left

1. Make the recon wavefront bit-exact on the qm stream above (`EC_AV1_RECON_THREADS=4`), then
   default it on inside the decoder. That is the path to ≥30 fps at 6 threads, measured at
   32.5 to 34.5 fps (single runs, unchanged code). The threading would need to be scoped per
   frame so the counter gates still see it under `cfg(test)`.
2. The frame-worker default is slightly slower than one thread on this 4-core host (20.6 vs
   21.0 fps, within noise), because 6 workers are oversubscribed on 4 cores.
3. Decode-side tile threading does not exist. `EC_AV1_TILE_THREADS` is encoder-only.
