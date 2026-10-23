# Lane stackflake: make `test_threads_get_the_stack_they_are_given` deterministic

## Goal
`stack_budget::tests::test_threads_get_the_stack_they_are_given` (ec-ac3 and ec-av1) failed
intermittently. Make it deterministic without weakening what it guards.

Base: branch `lane/stackflake` cut from `main` at `bfd5d20071588a36804053324662683bfd0b8d77`.

## Root cause (reproduced)
The explicit 1 MiB probe thread's stack mapping is found by parsing `/proc/self/task/<tid>/maps`.
Linux merges adjacent anonymous mappings with the same protections, so the mapping that holds the
thread's stack can grow past the stack's own size. The test had an upper bound
`size <= 512 MiB` ("not a stack"), which a merged mapping can exceed.

Reproduction, before the fix, locally (release, `cargo test --release -p ec-ac3 -p ec-av1 --lib
stack_budget`, 50 iterations): **5 of 50 failed**. All 5 were the same assertion:

```
thread 'stack_budget::tests::test_threads_get_the_stack_they_are_given' panicked at crates/ec-av1/src/stack_budget.rs:954:9:
the mapping holding an explicitly-1-MiB thread's frame is 537956352 bytes, which is not a stack -- the maps parse found the wrong region
```
(3 failures in ec-av1, 2 in ec-ac3; the ec-ac3 line is the same message at `stack_budget.rs:960`.)

The lower bounds (`size >= 1 MiB` for the explicit thread, `size >= 2 MiB` for the default test
thread) were never the failing ones. Merging only makes a region larger, so a floor stays valid.

## Changes
Both `crates/ec-ac3/src/stack_budget.rs` and `crates/ec-av1/src/stack_budget.rs`, same edit:
- `stack_region` now returns `(lo, hi, writable)`. `writable` is the second character of the maps
  permissions column. Merging joins only VMAs with matching protections, so a merged stack is
  still writable, and that check does not depend on the merged size.
- The `size <= 512 MiB` upper bound is replaced by `assert!(writable)` for the explicit thread and
  for the default test thread. It still rules out a wrong region (a PROT_NONE or read-only mapping).
- Lower bounds are unchanged: `size >= EXPLICIT` and `size >= 2 * 1024 * 1024`.
- No `libc` dependency was added. Neither crate depends on `libc`, so `pthread_getattr_np` was not
  an option under the lane's no-new-dependency rule.
- No other crate has this test. `ec-opus` and `ec-vp9` have `stack_budget.rs` but not this test.

## Verify
Loop counts, each the test `stack_budget::tests::test_threads_get_the_stack_they_are_given`:

| Setup | Before | After |
|---|---|---|
| Local, `cargo test --release -p ec-ac3 -p ec-av1 --lib stack_budget`, 50 runs | **5/50 failed** (513 MiB upper bound) | - |
| Local, `-p ec-ac3`, 50 runs | - | 0/50 |
| Local, `-p ec-av1`, 50 runs | - | 0/50 |
| server (auto, host a), `-p ec-ac3`, 50 runs | 0/50 (not reproduced) | 0/50 |
| server (auto, host a), `-p ec-av1`, 50 runs | - | 0/50 |
| server host a/b/c, release full ec-ac3 lib binary, 50 runs each | 0/50 each (no failure) | - |
| server host a, debug ec-ac3 binary, 4 concurrent copies x 50 | 0 failures in 200 | - |

The server runs before the fix did not reproduce the flake. The failure only showed up locally. The
pre-fix local loop is the reproduction.

Commands and results after the fix:
- `scripts/remote-run.sh stackflake --host auto -- cargo test --release -p ec-ac3 -p ec-av1 --lib stack_budget`:
  `8 passed; 0 failed` for each crate.
- `scripts/remote-run.sh stackflake --host auto -- cargo clippy -p ec-ac3 --all-targets -- -D warnings`:
  `Finished`, clean.
- `cargo clippy -p ec-av1 --all-targets` on the server: the only hit near `stack_budget.rs` is line 564
  (`eprintln!("{:>8}  {:>7}  {}", ...)`), which is outside the changed lines and pre-existing. The
  ec-av1 clippy failures belong to another lane.

## Notes
- The worktree already had an uncommitted change to `scripts/remote-run.sh` (3 job slots instead of 2)
  when this lane started. It is not from this lane and is not in the commit.
- `main` has advanced past the base sha above. The orchestrator merges.

## Left
- Nothing for this test. A bound-free check cannot catch a region that is writable but is not the
  thread's stack, such as a heap mapping. The probe address lives on the thread's stack, so such a
  mapping cannot contain it, and the test does not claim more than that.
