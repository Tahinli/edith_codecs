# Lane hevcframethreads: ec-h265 frame threading

Goal: frame threading for the ec-h265 decoder (and WPP wavefront for streams with
`entropy_coding_sync_enabled_flag`). Safe Rust, std only, no new dependencies. `EC_H265_THREADS`
(default min(cores, 6)) sets the thread count; `EC_H265_THREADS=1` is the serial path.

Base: `main` at `26c847dc`. Branch `lane/hevcframethreads`.

## Status

- Frame threading: done. Conformance is bit-exact at 1, 2, 4 and 6 threads.
- Targets (6 threads, local 12-core machine): 1080p 7.36 ms/frame (target 16.7, met);
  4K 27.11 ms/frame (target 33, met).
- WPP (`entropy_coding_sync_enabled_flag` wavefront): NOT done. Streams with WPP are decoded
  serially inside a picture; frame threads still apply to them.

## Commits

1. `8294d6a7` lane/hevcframethreads: in-loop filters per CTB row, pictures published row by row
   (serial decode, conformance 9203/9203 at 1 thread).
2. `cec7b004` lane/hevcframethreads: frame threads, one worker per picture; references wait for
   the rows they read (conformance 9203/9203 at 1, 2, 4, 6 threads).

## What changed

- `dec/shared.rs` (new): `SharedPic`, a picture published row by row. Final sample rows and the
  collocated motion (`ColField`) sit behind `RwLock`s; a `Mutex` + `Condvar` holds the progress
  counters (rows, col rows, done, error). Readers wait for the rows they need, then take the read
  lock. A failed picture wakes waiters with the error.
- `dec/rowfilter.rs` (new): deblocking and SAO run per CTB row, behind reconstruction. Deblock of
  row r needs row r+1 reconstructed; SAO of row r needs deblocked r+1. Finished rows are published.
- `dec/picjob.rs` (new): `PicJob` (one picture's state) and `SliceJob` (owned inputs for one slice
  segment, so the job can run on another thread).
- `dec/runner.rs` (new): `Runner` runs a picture inline (1 thread) or on its own worker thread.
  A worker panic or error marks the picture failed.
- `dec/mod.rs`: at most `threads` pictures in flight; the decoder joins the oldest when full.
  Outputs are resolved in order; `flush` waits for all pictures. The motion checksum folds the
  per-picture checksums in decode order, so it is the same at every thread count.
- `dec/inter.rs`: a prediction unit waits, per reference, for the rows it reads:
  `pu.y + pu.height + (mv_y >> 2) + 16` (at least one row, so a vector above the picture
  waits for row 0). The margin covers the 8-tap luma and 4-tap chroma filter extents.
- `dec/motion_field.rs`: temporal MV waits for the collocated rows of the CTB row it reads
  (`wait_col_rows`) and reads the published `ColField`.
- `dec/dpb.rs`, `dec/rps.rs`, `dec/cu_syntax.rs`, `dec/deblock.rs`, `dec/sao.rs`: ported to
  `Arc<SharedPic>`, per-row filtering and row callbacks.

Bug found and fixed during the work: the row bound for a reference used only the bottom of the
block. A motion vector pointing above the picture gives a bound of 0, the wait returned at once,
and the clamped reads at row 0 could see an unpublished row. This showed up as a checksum
difference on MVCLIP_A at 2 threads. The `max(1, ...)` clamp fixes it.

## Verify (host a, `scripts/remote-run.sh hevcframethreads --host a -- ...`)

Conformance, `EC_H265_REF_YUV_DIR=/home/user/refyuv/hevcinter EC_H265_THREADS=<n>
cargo test -p ec-h265 --release --test conformance_intra -- --nocapture`:

| threads | pictures exact | streams accepted | test time |
|---|---|---|---|
| 1 | 9203 / 9203 | 147 / 147 | 113 s |
| 2 | 9203 / 9203 | 147 / 147 | 90 s |
| 4 | 9203 / 9203 | 147 / 147 | 73 s |
| 6 (run 1) | 9203 / 9203 | 147 / 147 | 69 s |
| 6 (run 2) | 9203 / 9203 | 147 / 147 | 69 s |
| 6 (run 3) | 9203 / 9203 | 147 / 147 | 71 s |

Also: `cargo test -p ec-h265 -p ec-h265-syntax --release` passes (all test binaries, 0 failed);
`cargo clippy -p ec-h265 -p ec-h265-syntax --all-targets -- -D warnings` is clean.

## Bench (local, `decode_bench`, best of 3, `EC_H265_THREADS`)

Build: `CARGO_TARGET_DIR=~/.cache/tgt-hevcframethreads TMPDIR=~/.cache/tmp-hevcframethreads
cargo build --release -j4 -p ec-h265 --example decode_bench`.

| stream | 1 thread | 2 threads | 4 threads | 6 threads | target | met at 6 |
|---|---|---|---|---|---|---|
| hevc1080_moving (ms/frame) | 20.40 | 12.20 | 8.74 | 7.36 | <= 16.7 | yes |
| hevc4k_moving (ms/frame) | 79.76 | 49.17 | 35.01 | 27.11 | <= 33 | yes |

The checksums are identical at every thread count (1080p `2bfe18f415b7408d`, 4K
`6d7084bb34f40906`). The 1-thread numbers are higher than the earlier baseline (18.68 / 73.47 ms)
on this run; that is run-to-run machine variation, not a change in the serial path.

## What is left

- WPP wavefront for `entropy_coding_sync_enabled_flag` streams. This needs a parallel parse with
  per-row CABAC state handoff. Not started.
- 4K at 4 threads (35 ms) misses the 33 ms target; it is met only at 6 threads.
- The SAO helper `sao_picture_threads` in `dec/sao.rs` is an earlier threaded variant, kept for
  now. It is not on the decode path.
