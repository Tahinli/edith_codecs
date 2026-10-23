# Lane mp4fixturerace: race-free derived fixtures

## Goal

`crates/ec-mp4/tests/mp4.rs::a_fragmented_file_demuxes_to_the_same_packets_as_a_plain_one`
flaked once at mp4.rs:546 ("packet N: one side ended early"). The suspect was the
derived-fixture generator: `derived_fixtures()` checked `path.exists()` and then ffmpeg
wrote straight into the final path. Any test thread that saw the file while ffmpeg was
still writing it opened a half-written fixture.

Base sha: `011e0b4f11532aedb81e709fad6bab96f679a202` (main).

## Root cause

Both `crates/ec-mp4/tests/mp4.rs` and `crates/ec-matroska/tests/matroska.rs` built their
derived fixtures in a shared directory (`$TMPDIR/ec-mp4-tests`, `$TMPDIR/ec-matroska-tests`)
with this pattern:

```
if !path.exists() { ffmpeg -y ... <final path> }
```

- Test threads in the same binary run in parallel. Every `derived_fixtures()` caller
  races for the same files. Two callers that both saw "missing" ran ffmpeg -y on the same
  path at once.
- A caller that saw the file mid-write skipped generation and opened the partial file.
  `ffmpeg -movflags frag_keyframe+empty_moov` creates the file at the start and appends
  fragments as it goes, so the window is the whole copy.
- The matroska variant also left a partial file behind on ffmpeg failure, which a later
  run then treated as complete.

The reproduction shows the partial-file case directly (see below).

## Changes

1. `crates/ec-mp4/tests/mp4.rs`: `derived_fixtures()` now calls a new `make_derived(dst,
   args)`. It takes an exclusive lock (`File::lock`, stable since Rust 1.89) on
   `<name>.lock` beside the file, re-checks `exists()` under the lock, writes ffmpeg output
   to `tmp-<pid>-<name>` in the same directory (same extension, so the muxer is still
   inferred), and `rename`s it into place only after ffmpeg succeeds. On failure the temp
   file is removed. Readers therefore never see a partial fixture.
2. `crates/ec-matroska/tests/matroska.rs`: same helper and the same lock/temp/rename
   protocol in `derived_fixtures()`. A failed ffmpeg run still skips the file, as before,
   and no longer leaves a partial one behind.
3. `scripts/stress-mp4-fixtures.sh` (new): the multi-crate command from the flake
   (`-p ec-h265 -p ec-vorbis -p ec-probe -p ec-ac3 -p oxideav-ac3 -p ec-mp4 -p ec-image
   -p ec-alac -p ec-mp3 -p ec-ogg`), run three times and once more with
   `--test-threads=16`. Each run starts from a cold `$TMPDIR/ec-mp4-tests` and
   `$TMPDIR/ec-matroska-tests`, because the race only exists while those files are being
   made. It exits non-zero if any run fails.

`mux_entries.rs` was checked and left alone. Its work directories are per test name and
it removes them before each run, so no two tests share a file. The other ffmpeg writers in
`crates/ec-mp4/tests/mp4.rs` (`entry-*.mp4`, `esds.mp4`, `mp3-in-mp4.mp4`) also write per-test
names only. The `ec-aac`, `ec-ac3`, `ec-alac` and `ec-av1` temp files are keyed by process
id or are written by one test each.

## Reproduction (before the fix)

Host c, unfixed tree. Each run deletes `$TMPDIR/ec-mp4-tests`, then runs
`cargo test --release -p ec-mp4 --test mp4 -- --test-threads=16` while four `yes` processes
load the host.

- Result: **1 failure in 15 cold runs** (run 5).
- Failure: `a_fragmented_file_demuxes_to_the_same_packets_as_a_plain_one` panicked with
  `/home/user/tmp/mp4fixturerace/ec-mp4-tests/h264-fragmented.mp4: need more data`.
  That is a reader opening the fragmented file while ffmpeg was still writing it.
- Log on host c: `$HOME/tmp/mp4fixturerace/rep2-5.log`.
- Without load, 6 cold runs of the same command passed. The race is real but rare, so
  the low pass rate is expected, not evidence against the diagnosis.

I did not reproduce the exact mp4.rs:546 message from the original flake. The reproduction
hit the same test and the same partial-file cause through `Mp4Demuxer::new` instead.

## Verify commands and results

Run from the worktree through `scripts/remote-run.sh mp4fixturerace --host auto`.

| Command | Where | Result |
|---|---|---|
| `cargo clippy --all-targets -p ec-mp4 -p ec-matroska -- -D warnings` | host a | exit 0 |
| `cargo test --release -p ec-mp4 -p ec-matroska` | host a | all `test result` lines ok (ec-mp4 mp4 15 passed, 1 ignored; ec-matroska all ok) |
| `rustfmt --check --edition 2024` on both edited files | local | exit 0 |
| `scripts/stress-mp4-fixtures.sh` (runs 1, 2, 3 and `--test-threads=16`) | host a | run 1 PASS, run 2 PASS, run 3 PASS, run 4 PASS; 60 result lines per run, 0 FAILED/panicked lines |

Each stress run starts from a cold fixture directory, so `derived_fixtures()` generated
the fixtures in every run.

## Not done / what is left

- The post-fix runs were not under the CPU load that exposed the race. The reproduction
  rate was 1 in 15 under load, so four clean post-fix runs are only weak evidence that
  the race is gone. The fix removes the race by construction (lock, temp name, atomic
  rename), but a loaded post-fix loop of about 15 cold runs would show it stronger.
- Not pushed. Commit is local on `lane/mp4fixturerace`.
