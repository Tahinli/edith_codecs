# lane vp9speed: report

Base: `e7234d33` (main). Branch `lane/vp9speed`.
Goal: DONE.md ec-vp9 item "1080p30 profile 0 decodes at ≥ realtime (tile threading allowed)".

## Status

| Goal | State |
|---|---|
| 1080p30 `tile1080.ivf` decodes at ≥ realtime | Met with threads ≥ 2 (39.8 fps at 2, 49.6 fps at 4). One thread is 29.6 fps, 1% under 30. The default stays `EC_VP9_THREADS=1`. |
| libvpx 1080p30, 4 tile columns | 58.4 fps at 1 thread, 68.9 fps at 4. |
| Bit-exact output | Kept. Digest of `tile1080.ivf` is identical at every thread count and before/after. 305/305 libvpx vectors pass. |

## Measurements (host b, `scripts/vp9-bench.sh`, `decode_ivf`)

The fps column is decode-only: the time spent inside `Decoder::decode`. The old harness
also timed its FNV digest over every output sample, which cost about 2.5 s per 300 frames
and is not decoder work. Each value is the mean of two runs on a shared 4-vCPU host.

| clip | threads | before fps | after fps |
|---|---|---|---|
| tile1080.ivf | 1 | 18.87 | 29.61 |
| tile1080.ivf | 2 | 21.51 | 39.76 |
| tile1080.ivf | 4 | 24.05 | 49.58 |
| libvpx 4 tile cols | 1 | 45.79 | 58.42 |
| libvpx 4 tile cols | 2 | 44.00 | 61.55 |
| libvpx 4 tile cols | 4 | 42.57 | 68.90 |

Phase CPU seconds for 300 frames of `tile1080.ivf` at 1 thread (`EC_VP9_PROFILE=1`):

| phase | before | after |
|---|---|---|
| walk (parse + reconstruct) | 13.75 | 8.8 |
| of which MC (inter prediction) | 11.07 | 6.1 |
| tokens | 0.81 | 0.8 |
| inverse transform | 0.51 | 0.5 |
| loop filter (serial, whole frame) | 2.17 | 1.3 |
| tile-thread setup and merge (4 threads) | not measured | 6.25 before the buffer reuse, 0.67 after |

Thread overhead before the buffer reuse was dominated by allocating and zeroing a full
frame of planes per worker per frame. It is gone now.

## Changes

1. `mc.rs`: unscaled 8-tap horizontal and vertical passes (`conv_h_unscaled`,
   `conv_v_unscaled`). With an unscaled reference the filter is constant across a row or
   column, so each tap is one contiguous pass the compiler vectorises. Scaled prediction
   keeps the per-sample loops. A unit test compares the fast path with a per-sample
   reference for all 4 kernels, bit depths 8/10/12, all 16 subpel phases, block sizes
   4 to 64, and both averaging modes.
2. `transform.rs`: the 2D inverse transform uses stack scratch instead of `vec!` per call.
   Input rows that are all zero are skipped: every stage is linear and rounds 0 to 0, so
   the output row is already zero.
3. `loopfilter.rs`: luma runs on one thread and both chroma planes on another. The planes
   read the same grids and write disjoint buffers, so the output is unchanged.
4. `decode.rs`: a frame-buffer pool. Replaced reference frames are reclaimed when nothing
   else holds them (`Arc::try_unwrap`), and worker tile-thread planes come from the pool.
   Buffers are refilled to the same grey value a fresh allocation starts with.
   `stack_budget.rs`: the `Decoder` size inventory moved from 776 to 800 bytes for the
   new `spare` field.
5. `prof.rs` (new): phase timers behind `EC_VP9_PROFILE=1`. Off by default; one relaxed
   atomic load per timer when off.
6. `examples/decode_ivf.rs`: prints `decode_fps` (decode-only). The old `fps` still
   includes the digest and is kept for comparison.
7. `scripts/vp9-bench.sh` (new): builds the example and runs the 1/2/4 sweep on
   `tile1080.ivf` and on a libvpx 4-tile clip made with ffmpeg.
8. `lanes/bench.md`: rows appended.

## Verify (host b, `scripts/remote-run.sh vp9speed --host b`)

- `cargo clippy -p ec-vp9 --all-targets -- -D warnings`: exit 0.
- `EC_REQUIRE_FIXTURES=1 cargo test --release --no-fail-fast -p ec-vp9`: exit 0, 28 test
  result lines, all `ok`, 0 failed.
- `EC_REQUIRE_FIXTURES=1 cargo test --release -p ec-vp9 --test libvpx_vectors -- --nocapture`:
  `libvpx vectors: 305 checked, 4766 shown frames, 0 problems`.
- New test `mc::tests::unscaled_fast_path_matches_per_sample_reference`: passes.

## What is left

- Loop filter is 1.3 s of 300 frames (about 4 ms a frame), serial within luma. Splitting
  luma into superblock-row bands with a row sync, as libvpx does, is the next step if
  1-thread realtime is needed.
- MC is still the largest phase (6.1 s). The i32 accumulators do not map to SSE2
  `pmaddwd` cleanly. A `std::arch` path would need an `#[allow(unsafe_code)]` in one
  module with a runtime-detected scalar fallback; not done.
- Intra prediction still copies edge rows with `to_vec()` per transform block (`intra.rs`).
  Keyframes only, not on the inter path that dominates here.
- Per-block `MiInfo` is cloned into every covered mode-info cell (`decode.rs`). Not
  profiled separately.
- Host b is shared, so fps varies by about 5 percent between runs.
