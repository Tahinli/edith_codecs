# fuzzflac: cargo-fuzz targets for ec-flac

Goal: DONE.md shared gate 2, at least one cargo-fuzz target per untrusted entry point in
ec-flac (whole-file decode, per-frame decode, metadata parse, and an encode/decode roundtrip).
Base sha: 78ec1905 (main at worktree creation).

## What changed
- `crates/ec-flac/fuzz/`: standalone cargo-fuzz crate (`ec-flac-fuzz`, own `[workspace]`, path dep on ec-flac).
  - `decode_file`: whole input as a .flac buffer through `FlacReader::new` then `decode_all`.
  - `decode_frame`: byte 0 selects one of five fixed STREAMINFOs (as a container carries them); the rest
    is bare frames fed to `FlacReader::frames` and `next_block` until it stops. Asserts the decoded
    channel count and block length match the frame header.
  - `parse_metadata`: `StreamInfo::parse` on the whole input (asserts `to_bytes` reproduces the first 34
    bytes), then `FlacReader::new`, `seek_table`, and `seek_to_sample` at 0 and `u64::MAX`.
  - `encode_roundtrip`: header bytes pick channels (1..=8), bit depth (4..=32), rate, and block size; the rest
    is i32 samples clamped to the bit depth. `encode` then `decode_all` must return the input exactly.
  - `seed.sh`: every root `fixtures/audio/flac-*.flac` plus the xiph testbench (subset, uncommon, faulty) as
    decode_file and parse_metadata seeds (92 each); the same files with metadata stripped behind selector 0 as
    decode_frame seeds (90). `encode_roundtrip` has no seeds.
  - `.gitignore`: target, corpus, artifacts, coverage.
- `crates/ec-flac/src/decode.rs`: fix for the crash found by `parse_metadata` (see below). Regression test
  `seek_table_offset_overflow_does_not_panic` in the existing `tests` module.
- `crates/ec-flac/src/checksum.rs`, `src/lib.rs` (test), `tests/encode_matrix.rs`: `chunks_exact` replaced by
  `as_chunks` (same bytes, same remainder handling). Needed because the current clippy (rust 1.99) flags
  `chunks_exact` with a constant size; `-D warnings` failed on these before any change of mine.

## Crash found and fixed
- `parse_metadata`, libFuzzer: `attempt to add with overflow` at `decode.rs:335`, in `seek_to_sample`.
  A SEEKTABLE point with offset `u64::MAX` made `first_frame + p.offset as usize` overflow. Debug builds
  panic; release builds would wrap and move the read position to an arbitrary offset.
- Fix: the offset is converted with `usize::try_from` and added with `checked_add`; an out-of-range point is
  treated as "no such point" (seek to the first frame, answer 0), the same as the existing fallback.
- Regression test: `decode::tests::seek_table_offset_overflow_does_not_panic`. It was not run against the
  pre-fix code, so the test's failure before the fix is not observed.
- Fix attempts used: 1 (it passed the replay and the re-run on the first try).

## Verify commands and results
Run locally (see "Where it ran"): `CARGO_TARGET_DIR=~/.cache/tgt-fuzzflac TMPDIR=~/.cache/tmp-fuzzflac`, `CARGO_BUILD_JOBS=4`.
- `cargo +nightly fuzz build --fuzz-dir crates/ec-flac/fuzz`: exit 0 (all four targets built).
- `cargo +nightly fuzz run decode_file --fuzz-dir crates/ec-flac/fuzz -- -max_total_time=120 -rss_limit_mb=3000 -timeout=10`:
  exit 0, 4485 runs in 121 s, no crash.
- `... decode_frame ...`: exit 0, 3954 runs in 121 s, no crash.
- `... parse_metadata ...` (first run): exit 1, crash at seek_to_sample overflow (above). 251238 runs in the
  re-run after the fix: exit 0, no crash.
- `... encode_roundtrip ...`: exit 0, 652390 runs in 121 s, no roundtrip mismatch and no assert failure.
- Replay of the crash input against the fixed target: exit 0.
- `cargo clippy -p ec-flac --all-targets -- -D warnings`: exit 0 (after the chunks_exact to as_chunks change).
- `cargo test -p ec-flac --release -j4`: exit 0. Unit 16 passed (includes the regression test);
  integration `xiph_vectors` 2 passed; `encode_matrix` 4 passed; 0 failed.

## Where it ran
- The worker brief said to run through `scripts/remote-run.sh fuzzflac`. Host a was slot-busy for the whole
  10-minute window (exit 75) on two tries; host b was slot-busy too (exit 75, two windows). Per the
  coordinator's message, everything ran locally from the worktree after that. No server result exists for this lane.

## Left open
- The gate asks for 1 CPU-hour per target. Only the 120 s smoke runs were done, so the gate is not met.
- `FlacDecoder` (the `ec_core::Decoder` container path, STREAMINFO from extradata) has no dedicated target;
  `decode_frame` drives `FlacReader::frames` directly, which is the same frame decoder underneath.
- `encode_roundtrip` has no seeds; `decode_file` was not given the encoder's output as a seed.
- Corpus is seeded from fixtures only; no minimised corpus or coverage measured.
- `fuzz/Cargo.lock` was generated locally and is not in this commit (as in fuzzaudio1).
