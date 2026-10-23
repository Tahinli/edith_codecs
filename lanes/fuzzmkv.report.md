# Lane fuzzmkv: cargo-fuzz targets for the ec-matroska demuxer

## Goal
cargo-fuzz targets for the ec-matroska demuxer. Every crash found gets a fix and a regression replay test.

## Base
- Lane branch `lane/fuzzmkv`, merged `main` at `322837a2` (`git merge main` after the WIP commit).
- `main` had moved on to `5833f6d9` by the time of this report; this lane was not re-merged against it.

## What changed
- `crates/ec-matroska/fuzz/`: cargo-fuzz crate with seven targets (`demux_packets`, `ebml_walk`, `index`, `lacing`, `open_tracks`, `seek`, `tracks`), a seed script, and `.gitignore` for corpus and artifacts.
- `crates/ec-matroska/src/fuzz_api.rs` (`#[doc(hidden)]`): thin entry points for the targets.
- `crates/ec-matroska/src/demux.rs`: overflow-safe arithmetic for element offsets, cue and seek positions, block timestamps, lace sizes, and SAR; a `MAX_CHANNELS` (255) refusal for audio tracks; an `UNPACKED_LIMIT` (256 MiB) per cluster for inflated or header-stripped frames; `parse_tracks` now returns `Result`. Internal helpers are `pub(crate)` for the tests.
- `crates/ec-core/src/timebase.rs`: `checked_rescale` uses checked i128 multiplication.
- `crates/ec-matroska/tests/fuzz_regressions.rs`: replay tests for the crashes found so far.
- `crates/ec-matroska/tests/matroska.rs`: clippy-driven changes in the test helpers (`AudioTrack` type alias, `as_chunks`).

## Fuzz runs (1 CPU-hour per target)
Each target ran for `-max_total_time=3600` with its own process and core, four concurrently on host c and three on host b. The first attempt on both hosts never started (a script path bug); the runs below are the ones that count.

| Target | Host | Wall | Runs | Avg exec/s (last progress line) | Exit | Crash artifacts |
|---|---|---|---|---|---|---|
| demux_packets | c | 3601 s | 8,518,009 | 2365 | 0 | none |
| seek | c | 3601 s | 12,526,732 | 3478 | 0 | none |
| ebml_walk | c | 3602 s | 60,757,433 | 16872 | 0 | none |
| lacing | c | 3601 s | 277,069,449 | 76942 | 0 | none |
| open_tracks | b | 3602 s | 16,373,362 | 4546 | 0 | none |
| index | b | 3602 s | 114,021,080 | 31663 | 0 | none |
| tracks | b | 3602 s | 111,984,373 | 31098 | 0 | none |

No crashes, timeouts, or OOMs were found in these runs. No new fixes or replay tests were needed in this pass; the replay tests already in the branch cover the crashes found in the earlier session.

## Verify
Run on host c via `scripts/remote-run.sh fuzzmkv --host auto`:

- `cargo test --release -p ec-matroska -p ec-core`: exit 0. Results per test binary: 44 passed, 10 passed, 10 passed, 8 passed with 5 ignored (the ignores are pre-existing), and two empty binaries (doc tests). 0 failures.
- `cargo clippy -p ec-matroska -p ec-core --all-targets -- -D warnings`: exit 0, clean.

## Left
- Fuzz runs are 1 h each, so coverage is still short of a long soak. Re-run the targets on the next lane pass.
- Re-merge with `main` (now `5833f6d9`) before the lane merges.
- The first attempt at both fuzz waves failed silently (an unset path in the wave script) and was re-run; the hosts were clean before the final run.
