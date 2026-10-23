# fuzzmp4: cargo-fuzz targets for ec-mp4

Goal: DONE.md shared gate 2, at least one cargo-fuzz target per untrusted entry point in
ec-mp4 (container open and sample tables, fragments, sample reads, esds/codec config, sniffer).
Base sha: 78ec1905 (main at worktree creation).

## What changed
- `crates/ec-mp4/fuzz/`: standalone cargo-fuzz crate (`ec-mp4-fuzz`, own `[workspace]`, path deps on
  ec-mp4 and ec-core; ec-core is needed for the `Demuxer` trait). `.gitignore` for
  `target`, `corpus`, `artifacts`, `coverage`. The root `Cargo.toml` already excludes `crates/*/fuzz`.
  - `demux_mp4`: whole input as a file through `Mp4Demuxer::new`; lists tracks, duration, fragment
    count; `next_packet` until error; then `read_sample` for every index of every stream.
  - `esds_parse`: whole input as an `esds` payload through `Esds::parse`.
  - `is_mp4`: `is_mp4` sniffer on the whole input.
  - `seed.sh`: every mp4/m4a/mov/m4v fixture under `fixtures/audio` and `fixtures/video`. Files over 1 MiB
    are re-cut with `ffmpeg -t {0.2,0.05,0.01} -c copy` until they fit. `esds_parse` gets the payload of
    each seed's first `esds` box. `is_mp4` gets the first 64 bytes. Result: 41 demux_mp4, 12 esds_parse,
    41 is_mp4 seeds (13 MB corpus, git-ignored).
- `crates/ec-mp4/src/demux.rs`, two bugs found by the fuzzer, both fixed:
  1. **Fragment `trun` allocation (found at exec ~7.5k, fixed).** A `trun` with no per-sample fields
     (width 0) took its sample count from its own header, unchecked, and pushed that many samples.
     A 16-byte box claiming 0xFFFFFFF0 samples allocated the machine. Fix: a track's total sample count
     (moov table plus all fragments) may not exceed the file length, the same ceiling `stsz` already
     applies. Errors with `Error::corrupt`.
  2. **Channel count allocation (found at exec ~115k, fixed).** A QuickTime version-2 sound description
     states its channel count as a 32-bit field, which went straight into `ChannelLayout::from_count`,
     which allocates one channel position per channel. A 3.3 GB allocation from a 952-byte file. Fix: new
     `channel_layout()` helper refuses counts over `MAX_CHANNELS = 64` with `Error::corrupt`. Used for
     both the classic and version-2 paths.
- `crates/ec-mp4/tests/mp4.rs`: regression test `a_trun_claiming_more_samples_than_the_file_is_refused_not_allocated`
  (appends a crafted `moof` to the mono AAC fixture; must be an `Err`).
- `crates/ec-mp4/src/demux.rs` unit test `a_sound_description_stating_billions_of_channels_is_refused`
  (v2 entry with channel count 0xFFFFFFF0 is refused; 6 channels still builds a 5.1 layout).
- `crates/ec-mp4/tests/mp4.rs` line 249: `name_of(&path)` changed to `name_of(path)`. This was a
  pre-existing `clippy::needless_borrow` error that blocked the `-D warnings` gate, not part of the fuzz work.

## Verify commands and results
All through `scripts/remote-run.sh fuzzmp4`, from the worktree. Host a unless noted; host b and local were busy or
not used.
- `cargo +nightly fuzz build --fuzz-dir crates/ec-mp4/fuzz`: exit 0 (3 targets).
- `cargo +nightly fuzz run is_mp4 --fuzz-dir crates/ec-mp4/fuzz -- -max_total_time=120 -rss_limit_mb=3000 -timeout=10`:
  exit 0, 102,436,456 runs in 121 s, no crash. Run before the fixes; `is_mp4` does not reach the changed code.
- `... esds_parse ...`: exit 0, 18,809,040 runs in 121 s, no crash. Run before the fixes; `esds.rs` is unchanged.
- `... demux_mp4 ...` before fixes: OOM at exec 7,476 (trun allocation).
- `... demux_mp4 ...` after fix 1: OOM at exec 114,981 (channel count allocation).
- `... demux_mp4 ...` after both fixes: exit 0, 4,630,555 runs in 121 s, no crash, no OOM, no timeout.
- Reproducer replay after fix 2: `cargo +nightly fuzz run demux_mp4 ... <oom input>`: exit 0, input now handled.
- `cargo test -p ec-mp4 --release` (server, host a): exit 0. Lib 19 passed; mp4 integration 16 passed, 1 ignored
  (pre-existing); rotation 3 passed. Includes both new regression tests.
- `cargo clippy -p ec-mp4 --all-targets -- -D warnings` (server, host a): exit 0.
- `cargo fmt --check`: not run; rustfmt is not installed for the server toolchain.

Crashes found: 2 distinct (trun allocation; channel count allocation). Both fixed on the first attempt, both
have a regression test. Timeouts: none. Crash inputs are not committed; the regression tests carry the case.

## Left open
- The gate asks for 1 CPU-hour per target. Only 120 s smoke runs were done, so the gate is not met.
- `esds_parse` and `is_mp4` were smoke-run before the two demux fixes. Their code paths are unchanged, but a
  post-fix re-run has not been done.
- Only the first 4 KB of each `ec-mp4` sample entry is exercised per iteration; no per-codec entry seeds beyond
  the fixtures. Coverage was not measured.
- No minimised corpus; `fuzz/Cargo.lock` was generated on the host and is not in this commit.
- No `mux` target: the muxer writes only caller-supplied data, so it is not an untrusted entry point.
