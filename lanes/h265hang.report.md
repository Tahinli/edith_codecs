# Lane h265hang: deadlock in the ec-h265 frame-thread decoder

## Goal
Find and fix the deadlock behind the 6 h hang of `pixel_pipeline_runs_over_conformance_streams`
(host a, 5 worker threads and the main thread in `futex_do_wait`, 0% CPU).

Base sha: a3105413 (main).

## Root cause
`SharedPic::read_planes()` is a `RwLock` read. `InterRefs::predict` (crates/ec-h265/src/dec/inter.rs)
took one read lock per list. A B slice whose two lists name the same picture (common) therefore
took two read locks on that picture from the same worker thread. The picture's own worker
publishes rows with `planes.write()` while it is still decoding. Rust's futex RwLock queues
readers behind a waiting writer, so when the write was queued between the two read acquisitions,
the worker waited on a read lock that only it could release, and the writer waited for the first
read guard. Every other worker then waited on rows of that picture, and the caller joined a worker
in `start_picture` (the main thread in `futex_do_wait`). This is a lock-ordering deadlock, not an
error-path problem; the error paths already wake every waiter (`fail`, `run_worker`).

Why it is rare: the window is between two consecutive lock acquisitions in `predict`, so it needs
a row publish to land exactly there. Long runs on many threads hit it.

## Change
- `inter.rs`: new `lock_ref_planes(pics, between)` takes each distinct picture's read lock once.
  `predict` uses it and reads list 1 through the list 0 guard when both name one picture.
  `between` is a no-op in production; the test uses it to queue the writer in the window.
- `inter.rs` test `shared_reference_in_both_lists_locks_once`: a writer thread queues behind the
  first lock, then the second lock is requested. Runs on a thread with a 20 s `recv_timeout`
  watchdog, so a deadlock fails the test.

## Verify
Pre-fix (same test, naive helper that locks each list): deterministic failure on host b:
`prediction deadlocked: a reference picture was read-locked twice: Timeout`, 20.00 s, exit 101.
Post-fix (host c): `test result: ok. 1 passed; 0 failed` (exit 0).

- `cargo test --release -p ec-h265 -p ec-h265-syntax` (host a, EC_H265_REF_YUV_DIR=$HOME/refyuv/hevcinter):
  all test binaries `test result: ok`, 0 failed; exit 0.
- Conformance line (host a, `conformance_intra conformance_pictures_match_the_sei_hash -- --nocapture`):
  `total: 9203 pictures checked, 9203 exact, 147/147 streams accepted`
- `pixel_pipeline` loop, 50 iterations (host b, after the fix, `timeout 300` per run): no hang, no failure.
- `cargo clippy -p ec-h265 --all-targets -- -D warnings` (host c): clean, exit 0.

## Not done
- The error-injection loop (push a corrupt NAL mid-stream, drop, 1/2/4/6 threads) was not written.
  The deadlock does not depend on the error path, so the regression test targets the lock order.
- No pre-fix hang was reproduced on the real stream test; the deterministic test reproduces the
  mechanism instead.
