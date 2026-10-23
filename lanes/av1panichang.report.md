# Lane av1panichang: a worker panic ends the threaded decode with an error

Base: `a62e7dc2` (main). Branch `lane/av1panichang`.

## Goal

A panic on any worker of the threaded stream decode (`decode_stream_with_threads`
at `threads > 1`) must release every waiter and return an `Err` from the decode
call. It must never hang the process and never double-panic.

## Waiters found

Every place a worker result is awaited in `crates/ec-av1/src`:

| Waiter | Where | Released by | Hang before this lane |
|---|---|---|---|
| `collect_one`'s `rx.recv()` | `stream/mod.rs` | one channel answer per dispatched job | yes: a job that unwound past its `catch_unwind` never sent, and the frame thread holds a `Sender`, so `recv` never disconnects |
| `SlotPromise::wait` (condvar) | `stream/mod.rs` | `fulfil` / `fail` | yes, once a `fail` panicked on a poisoned lock, the failure was never reached |
| wavefront `wave_barrier` and dependent recon jobs | `decode/mod.rs` | `active` count and `done[r]` written by the worker | yes: a panic in `exec_op` unwound out of `wave_worker`, so `active` and `done[r]` were never updated, and `WaveGuard` drop waited forever |
| `PipeSync::wait_for` in `pipe_run_lr` | `decode/post_filter.rs` | `note` / `close` from `pipe_run` | yes: a panic in `pipe_run` never closed `lr_sync`, and `PipeGuard::finish` waits on the batch that holds `pipe_run_lr` |
| `Batch::drop` (pool join) | `par.rs` | the job wrapper's count decrement | a poisoned lock in the wrapper could skip the decrement; a re-raise during unwinding aborted the process |

## Changes

- `crates/ec-av1/src/stream/mod.rs`
  - `JobAnswer`: a drop guard that owns a job's channel sender and its promises. `send` answers normally and fails the promises on an error reply. `Drop` answers with `Error::corrupt("AV1 decode_stream: a worker thread panicked decoding a frame")` and fails both promises if the job is still unanswered.
  - Frame jobs and show-existing jobs run their whole body inside an outer `catch_unwind`, so no panic escapes into the pool's `Batch` (which would re-raise it on the parsing thread at the end of the decode).
  - `SlotPromise` locks go through a poison-tolerant `lock()`, so `fail` and `fulfil` cannot panic after another panic.
- `crates/ec-av1/src/decode/mod.rs`
  - `wave_worker` runs each recon job under `catch_unwind`. A panicked job sets `WaveQueue::failed`, still updates `active` and `done[r]`, and later jobs are drained without running, so the queue always empties.
  - `wave_check` returns `Err("AV1 decode: a reconstruction worker thread panicked while decoding this tile")` when `failed` is set. It is called at the end of each tile in `decode/frame_tile.rs` and `decode/key_tile.rs`, after `flush_recon` and before the `WaveGuard` drops.
- `crates/ec-av1/src/decode/post_filter.rs`: `UnwindAbort` guard in `pipe_run`. On unwind (`std::thread::panicking()`) it closes both `sync` and `lr_sync` with `abort`. A normal return does not abort, so the restoration worker is not cut short.
- `crates/ec-av1/src/par.rs`: `Batch`'s job wrapper and `Batch::drop` use poison-tolerant locks, so the count always comes down and the wait always ends. `Batch::drop` re-raises a job panic only when the current thread is not already unwinding, which removes the double-panic abort.
- `crates/ec-av1/src/fault.rs` (new, `#[doc(hidden)] pub mod fault` in `lib.rs`): test-only fault injection. `arm(site, nth)` panics the `nth` time a worker reaches a named site, once. Disarmed, a point is one relaxed atomic load. Sites: `frame` (inside a frame job's own catch), `frame-after` (past it, unwinding through the job), `recon` (a wavefront job), `filter` (a loop-filter band).
- `crates/ec-av1/tests/worker_panic.rs` (new):
  - `a_worker_panic_ends_the_decode_with_an_error_at_every_site`: for each site, decodes `fixtures/reelb_arf_witness.obu` at 4 threads on a helper thread, `recv_timeout` 60 s, and asserts an `Err` whose text contains `panicked`.
  - `a_decode_after_a_worker_panic_still_works`: an armed decode fails, then an unarmed decode at 4 threads succeeds and shows the same number of pictures as the 1-thread decode.
- `crates/ec-av1/src/stack_budget.rs`: `fault.rs` added to the by-value scan list. `every_by_value_module_is_scanned` requires every module `lib.rs` declares to be listed. The first full run failed on it, and the rerun is green.

Error text: the requested "worker panicked" wording is produced as `a worker thread panicked ...`, which keeps the `panicked` substring the test checks. The error kind is `Error::corrupt` in the existing `ec_core` helper style.

## Verification

Run on host b with `scripts/remote-run.sh av1panichang --host b`, `EC_MEMGUARD_MAX=6G`.

1. `cargo test --release -p ec-av1 --test worker_panic -- --test-threads=3`: 2 passed, 0 failed (7.4 s). All four sites were reached: each armed decode returned an `Err` (`expect_err` would fail on a site that was never hit).
2. Negative check: with the answer-on-unwind in `JobAnswer::drop` disabled, the same test fails with `the decode did not answer within 60s: it hung` (the `frame-after` site). The change was reverted afterwards. The recon and filter sites were not re-checked against the pre-fix code.
3. `cargo test --release -p ec-av1 --no-fail-fast -- --test-threads=3` (with `EC_AV1_AOM_DIR=$HOME/.cache/aom-oracle`, log at the lane scratchpad): every test binary `ok`.
   - lib: 816 passed, 0 failed, 49 ignored (3049 s).
   - `aom_conformance`: ok.
   - `fuzz_regressions`: 5 passed.
   - `worker_panic`: 2 passed.
   - `real_library`: 1 ignored (personal media, needs `--ignored`).
   - Doc tests: 0 failed.
   The first full run failed on `stack_budget::tests::every_by_value_module_is_scanned` (the new `fault.rs`). The fix above was applied and the run restarted, so the numbers above are from the clean run.
4. `cargo test --release -p ec-av1 --test aom_conformance -- --nocapture` with `EC_AV1_AOM_DIR=$HOME/fixtures/av1-aom EC_REQUIRE_FIXTURES=1`: `aom vectors: 242/242 exact (611 shown frames), 0 refused by name, 0 known failing, 0 failed`. The `~/.cache/aom-oracle` directory on host b has no vectors, so the test skips there and that run is not the gate.
5. `cargo clippy -p ec-av1 --all-targets -- -D warnings`: exit 0, 0 warnings.
6. Line counts: the largest file under `crates/ec-av1` is 4848 lines (`decode/rect.rs`). `decode/mod.rs` and `stream/mod.rs` are under 5000 after the change.

## Known limits

- A panic inside a loop-filter band (`par::Batch` on the filter pool) is re-raised on the thread that waits for the batch. Inside a threaded stream decode that thread is a frame job, so it becomes an `Err`. A direct single-thread stream decode that uses the filter pool would still see the panic propagate to its caller. It does not hang.
- `cargo fmt --check` is not clean on the crate at base `a62e7dc2` (many pre-existing diffs). The new and changed hunks were formatted by hand to rustfmt's output, and `stream/mod.rs` was clean at base and stays clean.
- Fault injection is process-global, so the two tests in `worker_panic.rs` take a mutex and run one at a time.
