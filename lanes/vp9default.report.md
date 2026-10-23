# lane vp9default: EC_VP9_THREADS default

## Goal
edith should get realtime 1080p VP9 decode without setting `EC_VP9_THREADS`. `lanes/vp9speed.report.md`
measured 29.6 fps at 1 thread and 39.8 fps at 2, so the default moves from 1 to the available
parallelism capped at 6.

Base sha: 1f2f6791 (main).

## Changes
- `crates/ec-vp9/src/decode.rs`: new `default_threads()` = `available_parallelism()` (1 if unknown),
  `.min(6)`. `Decoder::new` reads `EC_VP9_THREADS`; a zero value is now filtered out, and
  unparseable values are also ignored, so both fall back to the default. A positive value is
  still clamped to 1..=64, so `EC_VP9_THREADS=1` forces one thread.
- Doc comment on the `threads` field updated.
- `crates/ec-vp9/examples/decode_ivf.rs`: header doc updated to the new default.
- `fork()` (decode.rs, `threads: 1`) is left as is. It builds the per-tile-column worker decoder,
  which must not spawn its own workers. It is not a public default.

## Verify (run through `scripts/remote-run.sh vp9default --host auto`, host c)
- `EC_REQUIRE_FIXTURES=1 cargo test --release -p ec-vp9`: exit 0, every test result line
  shows 0 failed. `libvpx_vectors_match_per_frame_md5 ... ok` (the 305-vector md5 test ran, not
  skipped). Log: `scratchpad/vp9default-test.log`.
- `cargo clippy -p ec-vp9 --all-targets -- -D warnings`: exit 0.

## Not measured
- Realtime fps on the new default was not re-benchmarked in this lane. The expected numbers are
  the ones in `vp9speed.report.md`.
- The env parsing has no dedicated unit test. It was checked by reading the code.

## Left
- Nothing in this lane. A benchmark on edith's hardware is still worth doing.
