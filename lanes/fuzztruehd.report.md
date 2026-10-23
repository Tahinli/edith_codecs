# fuzztruehd: cargo-fuzz targets for ec-truehd

Goal: DONE.md shared gate 2, at least one cargo-fuzz target per untrusted entry point in ec-truehd.
Base sha: 78ec1905 (main at worktree creation).

## What changed
- `crates/ec-truehd/fuzz/`: standalone cargo-fuzz crate (`ec-truehd-fuzz`, own `[workspace]`, path dep on ec-truehd).
  - `decode_stream`: whole input as a TrueHD/MLP elementary stream. Split into access units by their
    own length words (`frame_length`), fed to one `TrueHdDecoder` so restart and filter state carries across units.
  - `parse_sync`: major sync only. Raw input through `AccessUnitHeader::parse`, then the same input
    wrapped in a valid AU header and TrueHD major sync word (the fixup, so mutations land on the sync
    fields and directory instead of the 4 sync bytes).
  - `seed.sh`: no TrueHD fixtures in the repo, so seeds come from ffmpeg's truehd/mlp encoders (stereo,
    2.1, 5.1 TrueHD and mono MLP, 1 s sine). `parse_sync` gets each stream's first access unit.
    ffmpeg's truehd encoder has no 7.1 layout, so 7.1 is not seeded.
  - `.gitignore` for `target`, `corpus`, `artifacts`, `coverage`.
- No checksum fixup in the targets: the restart CRC, substream parity and lossless check only increment
  `CheckStats` counters and never return an error, so mutated input is not rejected on those grounds.
- `crates/ec-truehd/src/decode.rs`, two fuzz-found panics, both fixed as `Error`s or guards:
  1. `read_filter` stored an out-of-range FIR/IIR order (9..15) before returning `Error::Corrupt`. The
     decode loop keeps going after an error, so a later access unit with no parameter block indexed past
     `fir.state`. Now validated before storing. Regression test: `out_of_range_filter_order_is_refused_and_not_stored`.
  2. `recompute_sign_offset` shifted by a negative `lsb_bits` when the quant step was above `huff_lsbs`
     (it runs before `huff_lsbs` is re-read). Now leaves the offset at 0; `read_block` already refuses that
     case with `Error::Corrupt`. Regression test: `quant_step_above_huff_lsbs_does_not_shift_negatively`.
- `crates/ec-truehd/src/encode.rs` and `tests/oracle.rs`: six pre-existing clippy findings under the
  current toolchain (`Iterator::find`, two `type_complexity`, `is_multiple_of`, a range loop over
  `ch_samples`, and three `chunks_exact` with constant size). Rewritten as iterator / `as_chunks` forms;
  the two `type_complexity` sites carry `#[allow]` with a reason. No behaviour change intended.

## Verify commands and results
Run locally, not through remote-run. Both server hosts reported `slot busy` through repeated 10-minute retry
windows (the first `remote-run` attempt on host a timed out after 600 s, the queued retry was stopped),
so the coordinator directed a local run with `CARGO_TARGET_DIR=~/.cache/tgt-fuzztruehd TMPDIR=~/.cache/tmp-fuzztruehd`.
- `cargo +nightly fuzz build --fuzz-dir crates/ec-truehd/fuzz`: exit 0.
- `cargo +nightly fuzz run decode_stream ... -- -max_total_time=120 -rss_limit_mb=3000 -timeout=10`:
  - run 1: panic at `read_filter`/`filter_channel` (index 8 of len 8). Crash found.
  - run 2 (after fix 1): panic at `recompute_sign_offset` (shift overflow). Crash found.
  - run 3 (after fix 2): exit 0, 30653 runs in 121 s, no crash. Saved inputs (30 minimised + 1 raw) all replay clean.
- `cargo +nightly fuzz run parse_sync ... (same flags)`:
  - run 1 and run 2: exit 0, 30668150 and 24623236 runs in 121 s, no crash.
  - run 3 (after both fixes): exit 0, 42904387 runs in 121 s, no crash.
- `cargo clippy -p ec-truehd --all-targets -- -D warnings`: exit 0 (after the encode.rs and oracle.rs fixes).
- `cargo test -p ec-truehd --release`: exit 0. lib 17 passed (includes the two new regression tests),
  library integration 1 passed, oracle 3 passed.
- `cargo test -p ec-truehd` (debug, overflow checks on): exit 0, 17 lib passed.

Crashes: 2 distinct (read_filter order OOB; recompute_sign_offset shift overflow). Each got one fix
attempt, and both fixes held through the final smoke runs. No timeouts or OOM.

## Left open
- The gate asks for 1 CPU-hour per target. Only 120 s smoke runs were done, so the gate is not met.
- `decode_stream` runs slowly (about 30k exec/120 s) because each input is a full stream; the corpus is
  seeded from short ffmpeg streams only, not from real Blu-ray TrueHD.
- No `ec-truehd/fuzz/Cargo.lock` in this commit (matches fuzzaudio1).
- ffmpeg's truehd encoder has no 7.1 layout, so 7.1 TrueHD is not in the seed corpus.
- `remote-run.sh` did not run any command: both hosts stayed slot-busy, so this lane has no server verification.
