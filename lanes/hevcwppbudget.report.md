# lane/hevcwppbudget: thread budget for WPP streams

## Goal
Make WPP streams use row parallelism under frame threading, lowering the 4 and 6 thread
times without slowing 1 thread or non-WPP streams.

## Base
`main` at `fae00c80`. **Result: no code change is committed.** None of the policies below
gave a measurable speedup, so the tree is left at main. This report records what was tried,
the measurements, and why.

## Bench streams
- `hevc1080_moving.hevc`, `hevc4k_moving.hevc` from `~/.cache/hevcperf-fixtures` (libx265,
  120 frames). Both have WPP on: the x265 SEI options string contains `wpp`
  (`strings`/`grep -a` on the files). Not regenerated with `-x265-params wpp=1`.
- Copied to host c at `~/bench-hevc/`. Host c has 4 cores and no hevcperf fixtures.
- Bench: `decode_bench` (best of 3, 120 frames), `EC_H265_THREADS` = 1/4/6.

## Measurements (host c, ms, best of 3)

Baseline runs are noisy on this host (other lanes share the box). The same code gave
1080p 4 threads 3846 and 2512 ms in two runs.

| stream | 1 thread | 4 threads | 6 threads |
|---|---|---|---|
| 1080p, main, run 1 | 5801 | 3846 | 2955 |
| 1080p, main, run 2 | - | 2512 | 1970 |
| 4K, main, run 1 | 23009 | 12302 | 10017 |
| 4K, main, run 2 | - | 8242 | 6804 |

Interleaved A/B (one remote run, same session; A = main behaviour, B = WPP frame cap at
`threads/2`), round 1:

| stream | threads | A main (ms) | B cap threads/2 (ms) |
|---|---|---|---|
| 1080p | 4 | 2430 | 3366 |
| 4K | 4 | 8098 | 12229 |
| 1080p | 6 | 1987 | 3095 |
| 4K | 6 | 6916 | 9954 |

Single-session runs of the same policies (noisy, same direction):

| policy | 1080p t4 | 4K t4 | 1080p t6 | 4K t6 |
|---|---|---|---|---|
| main (run 2) | 2512 | 8242 | 1970 | 6804 |
| frame cap threads/2 | 3327 | 11887 | 2995 | 9929 |
| frame cap 1 | 4171 | 15289 | 4475 | - |
| frame cap 2 | 3669 | 13439 | 3421 | 12628 |
| lend threads a blocked picture gives up (slice-start budget) | 2257 | 8232 | 2308 | 7175 |
| per-row helper lending, collocated wait included | 2553 | 8166 | 2551 | - |

All checksums matched the baseline (1080p `2bfe18f415b7408d`, 4K `6d7084bb34f40906`).

## Why it does not help (what was measured)
- CPU use. Main on 1080p: 4 threads, real 5.89 s, user 13.81 s (about 2.4 of 4 cores busy).
  6 threads: real 5.25 s, user 13.75 s (about 2.6 cores). So the cores are idle much of the time.
- Lending a blocked picture's thread did not raise CPU use (2.3 to 2.4 cores). Either the
  budget rarely has a free thread while pictures wait, or the idle time comes from the
  picture dependency chain and admission order, not from waits. Not fully diagnosed.
- Capping frames for WPP pictures removes frame parallelism and costs 30 to 50% (A/B above).
  Row parallelism inside one picture does not make up for it.
- The wavefront band path also has per-thread overhead (the lane report measured 6 to 9%
  slower at 1 thread), so extra row workers on a loaded box can cost more than they add.

## Verify (on main code, host a, `scripts/remote-run.sh hevcwppbudget --host a`)
`EC_H265_REF_YUV_DIR=$HOME/refyuv/hevcinter`

| command | result |
|---|---|
| `cargo test --release -p ec-h265 --test conformance_intra -- --nocapture`, `EC_H265_THREADS=1` | `total: 9203 pictures checked, 9203 exact, 147/147 streams accepted` |
| same, `EC_H265_THREADS=2` | same line |
| same, `EC_H265_THREADS=4` | same line |
| same, `EC_H265_THREADS=6` | same line |
| same, `EC_H265_THREADS` unset | same line |
| `timeout 1500 cargo test --release -p ec-h265 -p ec-h265-syntax` | pass, exit 0, all `test result: ok`, 0 failed |
| `cargo clippy -p ec-h265 --all-targets -- -D warnings` | clean, exit 0 |

## What is left
- Find the real idle time before changing policy. Trace per picture: time waiting on refs,
  time in admission (the `in_flight` join in `dec/mod.rs`), and time running rows.
- A larger change is probably needed: one shared task queue for the rows of all in-flight
  pictures, sized by the thread count, instead of a per-slice budget snapshot.
- Experiment patches (not committed) are in the session scratchpad:
  `wpp-budget.patch` (frame cap and first lending version) and `wpp-budget-dynamic.patch`
  (per-row lending).
