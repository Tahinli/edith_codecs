# fuzzriff: cargo-fuzz targets for ec-riff WAV and AVI readers

Goal: DONE.md shared gate 2, at least one cargo-fuzz target per untrusted parse entry point in
ec-riff (WAV/RF64 reader, AVI reader). ec-riff writer and public API unchanged.
Base sha: 78ec1905 (main at worktree creation).

## What changed
- `crates/ec-riff/fuzz/`: standalone cargo-fuzz crate (`ec-riff-fuzz`, own `[workspace]`, path dep on ec-riff).
  - `wav_read`: `WavReader::new`, then the metadata accessors, then a fresh reader drained with `read_all_i32` or `read_all_f32`.
  - `avi_read`: `AviReader::new`, `audio_streams`, `has_index`, `index_len`, then `next_packet` until error.
  - `avi_seek`: 10-byte control prefix (after flag, stream number, u64 LE target), then the AVI; calls `indexed_point`, `seek_to_stream_time`, drains packets, rewinds, drains again.
  - `seed.sh`: WAV fixtures from `fixtures/audio/*.wav` plus an RF64 variant of each (20 seeds); five synthesised AVIs for `avi_read` and `avi_seek` (5 + 10 seeds). No `.avi` fixtures exist in the repo.
  - `.gitignore`: `target`, `corpus`, `artifacts`, `coverage`.
- `crates/ec-riff/src/avi.rs` (fixes, all minimal, all in read paths):
  - `read_body` helper: reads a declared chunk body through `take()` instead of `vec![0; size]`. Used for `idx1`, `indx`, and the odml standard index. Fixes an OOM from an `idx1` size field of 0xC8000060 in a 2.5 KB file (found by `avi_read`).
  - `seek_to_stream_time`: `clamp` replaced by `min`/`max` (clamp panics when min > max on a movi list shorter than 8 bytes; found by `avi_seek`).
  - `seek_to_stream_time`: `movi_start + estimate` is now `saturating_add` (overflow for a huge target; found by `avi_seek` after the clamp fix, 2 further artifacts).
  - `queue_packets_in`: depth parameter, refused past `MAX_REC_DEPTH` (16). Nested `rec ` lists recursed once per level, so 100 000 levels overflowed the stack. Found by reading the code, not by the fuzzer.
  - `parse_odml_standard` and `chunk_matches`: `offset + 8` and `offset + 8 + size` use saturating arithmetic. Overflow for offsets near u64::MAX. Found by reading the code.
  - Clippy-only: `chunks_exact(16)` to `as_chunks::<16>()` in `parse_idx1_body`; the `Ok(... ?)` wrapper around the odml `collect` removed.
- `crates/ec-riff/src/read.rs`: `chunks_exact(4)` to `as_chunks::<4>()` in `read_all_f32` (clippy only).
- `crates/ec-riff/tests/avi_fuzz_regressions.rs` (new): five regression tests, each a hand-minimised inline AVI, not the fuzzer's artifact bytes:
  - `idx1_size_past_end_of_file_is_refused`
  - `seek_in_empty_movi_does_not_panic`
  - `seek_to_huge_target_does_not_overflow` (debug build panics without the fix, confirmed)
  - `deeply_nested_rec_lists_are_refused_not_recursed`
  - `odml_index_offset_at_u64_max_does_not_overflow`

## Crashes found and fixed
1. `avi_read`: OOM, `malloc(3355443296)` from the `idx1` size field. Fixed by `read_body`.
2. `avi_seek`: panic `min > max` in `clamp` (movi list shorter than 8 bytes). Fixed.
3. `avi_seek`: panic `attempt to add with overflow` at `avi.rs:211` (huge target). Fixed with `saturating_add`. Two artifacts (`crash-766d26...`, `crash-87ddd2...`) reproduced it; both pass after the fix.
4. Found by reading, not fuzzing: unbounded recursion on nested `rec ` lists; odml/chunk offset overflow. Fixed, with regression tests.

## Verify commands and results
Run from the worktree `~/.cache/wt/fuzzriff`, on branch `lane/fuzzriff`.
- `scripts/remote-run.sh fuzzriff -- cargo clippy -p ec-riff --all-targets -- -D warnings`: exit 0 (final run; ec-riff re-checked).
- `scripts/remote-run.sh fuzzriff -- cargo test -p ec-riff --release`: exit 0. 5 regression tests passed, 11 WAV tests passed, 0 doctests.
- `cargo test -p ec-riff --test avi_fuzz_regressions` (debug, overflow checks on): 5 passed. With the seek saturating_add reverted, `seek_to_huge_target_does_not_overflow` fails with the overflow panic.
- `cargo +nightly fuzz build --fuzz-dir crates/ec-riff/fuzz` (remote-run, host b): exit 0, all three targets built.
- Smoke runs, `cargo +nightly fuzz run <t> --fuzz-dir crates/ec-riff/fuzz -- -max_total_time=120 -rss_limit_mb=3000 -timeout=10`, final code:
  - `wav_read`: exit 0, 7274910 runs in 121 s. (An earlier pre-fix run on host b: 734011 runs in 121 s.)
  - `avi_read`: exit 0, 17918460 runs in 121 s.
  - `avi_seek`: exit 0, 15320442 runs in 121 s.
- The smoke runs were local (cargo-fuzz with a separate target dir). The vps hosts were slot-busy, so remote-run fell back to local.
- Replays of the crash artifacts on the fixed code: `avi_read` OOM artifact exit 0; `avi_seek` artifacts `crash-9299668...`, `crash-766d26...`, `crash-87ddd2...` exit 0.

## Left open
- The gate asks for 1 CPU-hour per target. Only 120 s smoke runs were done, so the gate is not met.
- Corpus is seeded from fixtures and synthesised AVIs only. No minimised corpus, no coverage measured.
- The `rec ` depth fix and the overflow fixes in `chunk_matches` and `parse_odml_standard` were found by reading, not fuzzing; the fuzzer did not reach them in the smoke runs.
- RF64 reads still return Unsupported (not a crash). The RF64 seed is only a read-path input.
- `crates/ec-riff/fuzz/Cargo.lock` was generated on the host and is not committed (same as fuzzaudio1).
- The regression reproducers are hand-minimised inline constructions, not the fuzzer's artifact bytes.
