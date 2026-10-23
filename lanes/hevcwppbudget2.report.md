# lane/hevcwppbudget2: measured idle time, then one-picture slack

## Goal
Find what leaves the ec-h265 threads idle on WPP streams at 4 threads, fix it, and keep
output bit-exact.

## Base
`main` at `5bc4e156`. Branch `lane/hevcwppbudget2`.

## Commits
- `43941c90` EC_H265_TRACE per-thread phase timing (off by default).
- `8cd58349` split the collocated wait into its own trace phase; one spare picture in flight.
- `70cbea72` spare picture only when threads fit the cores.
- `50671648` in-flight limit computed once per decoder (final code).

## Trace (EC_H265_TRACE=1, host b, 4 threads, timed run, ms summed over threads)

| stream | wall ms | role | threads | decode | wait_rows | wait_col | wait_admit | wait_sync | wait_msg |
|---|---|---|---|---|---|---|---|---|---|
| 1080p | 2279 | caller | 1 | 0.2 | 63 | 0 | 2009 | 0 | 0 |
| 1080p | 2279 | picture | 120 | 8574 | 1917 | 260 | 0 | 83 | 0.5 |
| 1080p | 2279 | wpp-helper | 4 | 278 | 114 | 0.5 | 0 | 89 | 0 |
| 4K | 7953 | caller | 1 | 0.4 | 184 | 0 | 6746 | 0 | 0 |
| 4K | 7953 | picture | 120 | 27946 | 4965 | 184 | 0 | 71 | 11 |
| 4K | 7953 | wpp-helper | 1 | 232 | 80 | 0.4 | 0 | 97 | 0 |

"decode" for picture threads includes the wait_rows and wait_col inside it.
Picture busy time (decode minus waits) is about 6.7 s of a 9.1 s thread budget (1080p).

Reading: the four picture threads are busy or blocked on reference rows about 94% of the
time. `wait_rows` (reference rows not yet decoded, MC) is the largest idle category, about
21 to 25% of picture-thread time. The CABAC row-sync wait and the message wait are under
1%. Collocated waits are about 3%. The caller's admission wait is the expected result of
waiting for a free slot, not lost capacity.

Ruled out: host contention. On host c, load average was 2 from other users, and a
single run showed inflated per-picture time (49 ms alone vs 84 ms at 4 threads). On host b
(4 cores, idle), four concurrent 1-thread decodes ran at the same speed as one alone
(about 45 to 47 ms/frame each vs 48), so the box does not slow parallel runs. Host c
numbers are not used.

Ruled out as the main cost: WPP end-of-segment row publication. The WPP path publishes rows
only after its segment merges (`cu_syntax/wpp.rs`), but the wpp-helper threads run only a
few ms to hundreds of ms per run, because the budget rarely has spare threads.

## Cause and change
With exactly `threads` pictures in flight, a picture blocked on reference rows holds its
thread, so the next ready picture cannot start. Allowing one spare picture in flight
(`PICTURE_SLACK = 1`, in `dec/mod.rs`) lets it start. Two spares measured no better. The
limit is applied only when `threads <= available_parallelism()`. Six threads on 4 cores
got slower with the spare (4 to 8%), so the spare is dropped there.

Safety: the oldest live picture only waits on pictures decoded before it, all finished,
so it always progresses. This is the same argument as the existing comment in `runner.rs`.

## A/B (host b, trace off, EC_BENCH_RUNS=1 per run, interleaved main then lane, 7 pairs)
Main binary: detached worktree at `5bc4e156`. Lane binary: `50671648`. Checksums matched
on every run (1080p `2bfe18f415b7408d`, 4K `6d7084bb34f40906`).

| threads | stream | main median ms | lane median ms | lane/main |
|---|---|---|---|---|
| 4 | 1080p | 2246.8 | 1916.2 | 0.853 |
| 4 | 4K | 7747.6 | 6667.2 | 0.861 |
| 6 | 1080p | 1930.6 | 1950.4 | 1.010 (noise) |
| 6 | 4K | 6681.8 | 6755.8 | 1.011 (noise) |

An earlier 2-thread check (5 pairs, before the core gate and final code) gave lane/main of
0.80 (1080p) and 0.72 (4K). It is not in the final table.

## Verify (host a, `scripts/remote-run.sh hevcwppbudget2 --host a`, with
`EC_H265_REF_YUV_DIR=$HOME/refyuv/hevcinter`)

| command | result |
|---|---|
| `cargo test --release -p ec-h265 --test conformance_intra -- --nocapture`, `EC_H265_THREADS=1` | `total: 9203 pictures checked, 9203 exact, 147/147 streams accepted` |
| same, `EC_H265_THREADS=2` | same line |
| same, `EC_H265_THREADS=4` | same line |
| same, `EC_H265_THREADS=6` | same line |
| same, `EC_H265_THREADS` unset | same line |
| `timeout 1500 cargo test --release -p ec-h265 -p ec-h265-syntax` | exit 0, 18 `test result: ok` lines, 0 FAILED |
| `cargo clippy -p ec-h265 --all-targets -- -D warnings` | exit 0, clean |

Note: the first full-suite run printed its summary through awk, so its exit code was
awk's. It was re-run with cargo's real exit code and reported exit 0 with no FAILED lines.

## What is left
- The 4-thread speedup is about 14 to 15%, not the 2.6x-of-4-cores a full overlap would give.
  Picture threads are still blocked on reference rows about 21 to 25% of the time. Removing
  that needs per-row reference progress in a finer order, or a larger change to admission.
- WPP row publication still happens at segment end. It only matters when spare threads
  exist, which is rare, so it was not changed.
- The trace hooks cost one cached flag check each when off. No clock is read when off.
- The 6-thread result is neutral, not faster.
