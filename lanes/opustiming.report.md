# lane/opustiming report

Base: add3513a (main at lane start). Branch: lane/opustiming. Not pushed.

## Goal

`encode_speed` and `decode_speed` in `crates/ec-opus/tests/conformance.rs` failed when the
full `cargo test --release -p ec-opus` ran in parallel on a busy host, though they passed
alone. Make the default suite a reliable gate. Floors unchanged, no `#[ignore]`.

## Changes

- `crates/ec-opus/tests/conformance.rs`
  - `thread_cpu_secs()`: CPU time of the calling thread via `clock_gettime(CLOCK_THREAD_CPUTIME_ID)`
    on Linux. Elsewhere the timer falls back to wall time.
  - `best_of_3()`: the fastest of three timed runs is used for every floor, including the
    5.1 decode check and the testvector01 stereo check.
  - `TIMING` static `RwLock`: `decode_speed` and `encode_speed` take it exclusively; every
    other `#[test]` in the binary takes it shared. A floor is never measured while another
    conformance test runs in the same process. The first attempt (CPU time + best-of-3
    alone) failed once: 5.1 decode 101x against the 104x floor on host c at load ~6.
  - No floor values changed. No test `#[ignore]`d.
- `crates/ec-opus/Cargo.toml`: `libc = "0.2"` dev-dependency (already in Cargo.lock at 0.2.189).
- `Cargo.lock`: adds `libc` to the `ec-opus` dependency list only.

## Verify (all on server via `scripts/remote-run.sh opustiming --host auto`, all landed on host c)

| Command | Result |
|---|---|
| `cargo clippy -p ec-opus --all-targets -- -D warnings` | exit 0 |
| `EC_REQUIRE_FIXTURES=1 cargo test --release -p ec-opus` run 1 | lib 42 passed; conformance 29 passed, 0 failed, 17 ignored; malformed_input 3 passed; exit 0 |
| same, run 2 | lib 42 passed; conformance 29 passed, 0 failed, 17 ignored; malformed_input 3 passed; exit 0 |
| same, run 3 | lib 42 passed; conformance 29 passed, 0 failed, 17 ignored; malformed_input 3 passed; exit 0 |

Fixtures: `scripts/link-fixtures.sh` run in the worktree; `EC_REQUIRE_FIXTURES=1` set, so no
fixture can skip silently. The 17 ignored tests are the same ones as before (corpus or probe tests).

Measured numbers, runs 1-3 (CPU time, best of 3, full suite in parallel):

| Config | Floor | Run 1 | Run 2 | Run 3 |
|---|---|---|---|---|
| encode mono 96k | 100x | 322x | 311x | 311x |
| encode stereo 128k | 70x | 193x | 187x | 189x |
| encode stereo 256k | 70x | 134x | 133x | 134x |
| encode stereo 510k | 50x | 106x | 107x | 104x |
| encode 5.1 384k | 25x | 65x | 63x | 59x |
| decode stereo opus-ogg | 135x | 212x | 221x | 223x |
| decode 5.1 opus-ogg | 104x | 110x | 110x | 114x |
| decode testvector01 stereo music | 135x | 202x | 239x | 239x |

## Note on the 5.1 numbers

- The 5.1 encode floor in the code is 25x, not 100x. The 100x floor is mono 96k encode. 5.1
  encode at 384k measures 59-65x and passes its 25x floor. Nothing was lowered.
- The 5.1 decode floor is 104x. Alone it measures 110x (lane opusgate reported 120x), so the
  margin is thin: about 6% under no load and 0% under load, which is why the lock was needed.

## What is left

1. 5.1 decode is close to its 104x floor (110-114x measured). Any slowdown in the decoder
   fails this gate. Encoder/decoder speed work is out of scope here.
2. The speed tests still depend on host load from other lanes (host c runs at load ~6 on 4 cores).
   The lock removes in-process contention only; neighbouring lanes can still slow the test thread.
3. Opusgate items not touched here: the multi-frame (40/60 ms) stereo SILK budget and the RFC 8251 `m` set.
