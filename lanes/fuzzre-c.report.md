# lane fuzzre-c: DONE.md G2 fuzz re-run on host c

Base: `main` at `f3e6c7fa`, branch `lane/fuzzre-c`. Host: c (4 cores, cargo-fuzz 0.13.2, nightly 1.101.0). Every run is
`cargo +nightly fuzz run <target> -- -max_total_time=<s> -rss_limit_mb=1536 -timeout=30`, seeded from the corpus
(`crates/<crate>/fuzz/seed.sh`, built from `fixtures/audio`). Logs: `~/tmp/fuzzre-c/<crate>-<target>.log` on c, with
`START`, `RESUME`, `END` and `EXIT <code>` lines.

Gate: at least 3600 CPU-s per target. `/usr/bin/time` is not installed on c, so CPU seconds come from
`~/tmp/fuzzre-c/cpu-samples.txt`, a sampler that records each libFuzzer process's `cputimes` every 30 s. Each figure
is the last sample before the process exited, so it can be up to about 30 s low.

## Results

| Target | Runs (wall s) | CPU s (total) | Crashes | EXIT | Final DONE (cov / ft) | Tree |
|---|---|---|---|---|---|---|
| ec-ac3 `decode_stream` | 664,725 (3601 s) + 698,028 resume (2201 s) | 2314 + 2040 = 4354 | 0 | 0, 0 | cov 3268 / ft 17694 | unchanged crate |
| ec-ac3 `parse_header` | 158,730,166 (3601 s) + 129,240,609 resume (2201 s) | 2328 + 2166 = 4494 | 0 | 0, 0 | cov 521 / ft 977 | unchanged crate |
| ec-truehd `decode_stream` (fixed tree, clean run) | 1,474,836 (3601 s) + 57,264 resume (121 s) | 3556 + 95 = ~3651 | 0 | 0, 0 | cov 939 / ft 5074 | fixed in this commit |
| ec-aac `decode_adts` | 188,806 (3601 s) + 21,152 resume (301 s) | 3447 + 289 = 3736 | 0 | 0, 0 | cov 1582 / ft 6713 | unchanged crate |
| ec-aac `decode_raw` | 120,458 (3601 s) + 12,563 resume (301 s) | 3451 + 284 = 3735 | 0 | 0, 0 | cov 1654 / ft 7477 | unchanged crate |

Notes on the runs:
- The first 1 h runs started 05:06 UTC (ec-ac3 both, ec-truehd); the two ec-aac runs started 06:06 UTC. All ran at 3 jobs at a time.
- ec-ac3 `decode_stream`, `parse_header` and ec-truehd were short of CPU after 3600 s wall (host c load was 8 to 21), so each was
  resumed from its corpus for the shortfall. ec-aac `decode_adts` and `decode_raw` were resumed for 300 s each.
- The ec-ac3 and ec-aac runs have a 3600 s wall limit; the CPU total is the sum over the run and its resume.
- Tree: the commit changes only `crates/ec-truehd`. The ec-ac3 and ec-aac crates are identical to `f3e6c7fa`; their
  runs are valid on the commit tree. The ec-truehd rerun ran on the commit tree (the tree after the fix below).

## ec-truehd crash (new in this rerun)

- A resume of the pre-fix ec-truehd run (07:07 UTC, 2200 s) hit `attempt to subtract with overflow` at
  `crates/ec-truehd/src/decode.rs:340` (`off -= 7 << lsb_bits` in `recompute_sign_offset`), then exited 1 at 07:38.
  The first 1 h run had been clean.
- Cause: `huff_lsbs` is stored before its `> 24` check. An access unit that fails that check leaves the value up to 31.
  The next access unit runs `recompute_sign_offset` with that stale value before `huff_lsbs` is re-read, and
  `7 << 31` overflows i32.
- Fix: `recompute_sign_offset` returns `Result<()>`. `lsb_bits > 24` is `Error::corrupt("TrueHD: huff_lsbs above 24")`.
  Both call sites use `?`. The `lsb_bits < 0` path is unchanged.
- Regression tests:
  - `crates/ec-truehd/src/decode.rs`, `stale_huff_lsbs_above_24_is_corrupt_not_an_overflow` (unit).
  - `crates/ec-truehd/tests/fuzz_regressions.rs`, replays `crates/ec-truehd/tests/data/fuzz/recompute-sign-offset-overflow.bin`
    (11061 bytes, sha256 `33d4aac2...ce7b`) through the fuzz target's loop.
- Gates (run on c, `CARGO_TARGET_DIR=~/.cache/tgt-fuzzre-c`):
  - `cargo test --release -p ec-truehd`: exit 0, all suites pass (18 + 2 + 2 + 1 + 1 + 3 and the doc-tests).
  - `cargo test -p ec-truehd --test fuzz_regressions` (debug, overflow checks on): exit 0.
  - `cargo test -p ec-truehd --lib stale_huff` (debug): exit 0.
  - `cargo clippy -p ec-truehd --all-targets -- -D warnings`: exit 0.
  - `crates/ec-truehd/src/decode.rs` is 1011 lines (under the 5000-line limit).
- The 1 h rerun on the fixed tree: `EXIT 0`, no panic, no artifact. The resume after it (120 s) also exited 0.

## Open point: ec-truehd coverage

- The fixed-tree run plateaued at cov 938 to 939 (ft about 5070). The pre-fix run reached cov 3087 (ft 19188).
  Replaying the full 6008-file corpus once through the fixed binary gives cov 939, so the decoder reaches less of its
  code with the fixed binary.
- The corpus is not short: the run loaded 6008 seeds (max 57824 bytes, the target's `lim`). The `INITED` coverage
  tracks the seed count in the earlier runs (386 at 4 seeds, 723 at 3055, 925 at 4918), so the pre-fix runs were
  not starting from a higher baseline; their growth came from fuzzing.
- The pre-fix binary cannot be replayed for comparison: it panics at `decode.rs:340` on a corpus seed (replay exit 77,
  built from `f3e6c7fa`), so its replay stops early.
- My reading: the change only affects access units that follow a refused one with stale state; valid streams never reach
  `lsb_bits > 24`. So the pre-fix growth to about 3000 probably came partly from those dirty-state sequences. I have not
  proven this. It needs checking before ec-truehd coverage is called a full result.

## Commands and verify results

| Command | Where | Result |
|---|---|---|
| `scripts/link-fixtures.sh`; `crates/{ec-ac3,ec-truehd,ec-aac}/fuzz/seed.sh` | local | corpora: ac3 37 seeds, truehd 11, aac 17 (gitignored) |
| `scripts/remote-run.sh fuzzre-c --host c -- ...` (sync, launch, resume, test, clippy) | c | see above |
| `cargo test --release -p ec-truehd` | c | exit 0 |
| `cargo test -p ec-truehd --test fuzz_regressions` (debug) | c | exit 0 |
| `cargo clippy -p ec-truehd --all-targets -- -D warnings` | c | exit 0 |

## Commit

- Branch `lane/fuzzre-c`, from `main` at `f3e6c7fa`. Changes: `crates/ec-truehd/src/decode.rs`,
  `crates/ec-truehd/tests/fuzz_regressions.rs`, `crates/ec-truehd/tests/data/fuzz/recompute-sign-offset-overflow.bin`,
  `lanes/fuzzre-c.report.md`. No push.
- Tree hash of `crates/` at the commit: `b3bfe098b2221e30ad65b9c5aebcf8e600ce1872` (`git rev-parse HEAD:crates`).
