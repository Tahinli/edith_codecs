# lane aacgates: ec-aac cannot panic on malformed input, and passes clippy

Base: `main` at 95bc8a76.

## Changes

1. `crates/ec-aac/src/sbr_hf.rs`: `limiter_band_table` returns `Result`. The three
   unwraps (patch border, `table[0]`, `out.last()`) are now `Error::corrupt` errors,
   propagated with `?` through `sbr_chain.rs`, `SbrChain::apply`/`apply_last` (now
   return `Result`), and `decode.rs`.
2. `crates/ec-aac/src/sbr_payload.rs`: the two `expect`s on header and tables are
   `ok_or_else(Error::corrupt)`.
3. `crates/ec-aac/src/lib.rs`: `AacDecoder::decode` clamped the ADTS body slice
   (`at + header_len` could pass `end` on a truncated frame and panic). The plane copy
   into `samples` now takes at most `frames` samples, since merged planes can differ in
   length.
4. Clippy (`-D warnings`, toolchain 1.98.1 on the servers) found a large set of issues
   that were not from this lane's changes:
   - `ec-dsp/src/lib.rs` `scale`: `chunks_exact` replaced by `split_at_mut` + `chunks_mut`.
   - 645 `excessive_precision` in `sbr_qmf_window.rs` and `sbr_qmf.rs`: literals rewritten
     to the shortest decimal that round-trips to the same f64 (checked bit-for-bit in the
     script that did it, `float(new) == float(old)`). No value changed.
   - `chunks_exact(CONST)` -> `as_chunks` (sbr_chain.rs, test files).
   - `encode.rs` `quantise`: `#[allow(clippy::too_many_arguments)]` with a reason line.
     It is on the encoder, not the decode path.
   - Test-only: unused `Result` from `apply` (now `.unwrap()`), a `useless_conversion`,
     a collapsible `if`, and a `needless_range_loop`.
5. New test `crates/ec-aac/tests/no_panic.rs`:
   - `fixtures_truncated_and_corrupted_never_panic`: the 6 committed ADTS fixtures in
     `fixtures/audio`, 10 truncation points each and 50 single-byte corruptions each,
     seed `SEED = 0x5EED_AAC0_0D15_EA5E`. Each input is decoded in `catch_unwind`.
   - `random_adts_frames_never_panic`: 500 frames, each with a valid 7-byte ADTS header
     (random sf_index 0..12, random channel_config 0..7, random length 7..606) followed by
     random payload.
   - Both pass. Note that the test verifies no panic only. It does not check that
     decode returns `Err` for corrupted input.

## Audit of the decode path (not exhaustive)

Grep for `unwrap()`, `expect(`, `panic!`, `unreachable!`, `assert!` in non-test code of
ec-aac. Remaining hits:
- `sbr_hf.rs`, `sbr_chain.rs`, `sbr_env.rs`, `sbr_payload.rs`, `sbr_bands.rs`, `config.rs`,
  `sbr_qmf.rs`: every `unwrap`/`expect`/`assert` hit left is inside `#[cfg(test)]`.
- `decode.rs`: `max_sfb` is validated against the band table in `parse_ics` (line ~184),
  and `section` runs are checked against `max_sfb`. The `cb[g][sfb]`, `sf[g][sfb]` and
  `group_len[g]` indexing in the spectral and TNS paths relies on that validation.
- `huffman.rs` `nodes[at][bit]`: the tree is built from fixed codebooks and walked only
  with bits 0/1 from the reader.

Not every index expression in the 16k-line crate was reviewed by hand. The no-panic test
is the evidence for input-reachable indexing, and it found nothing beyond the items above.

## Verify

- `scripts/remote-run.sh aacgates -- cargo clippy -p ec-aac --all-targets -- -D warnings`
  exit 0 (host a, run after the last source edit).
- `EC_REQUIRE_FIXTURES=1 scripts/remote-run.sh aacgates --host a -- cargo test -p ec-aac --release --no-fail-fast`
  - lib: 43 passed
  - reelh_per_frame: 1 passed
  - no_panic: 2 passed (new)
  - obs_real_content: 1 passed
  - oracle: 13 passed, 1 ignored (pre-existing `#[ignore]`)
  - sbr_real_library: 16 passed
  - exit 0
  I did not run `main` separately to diff test counts. Counts are compared against the
  pre-existing set only by the list above.

## Environment notes (not code)

- The servers had no clippy component on the 1.98.1 toolchain. I ran
  `rustup component add clippy` on both hosts.
- The worktree's `fixtures` symlink is absolute to the local machine. The sync copies it
  as is, so it dangles on the host. The run command re-points it to `~/fixtures`.
- The HE-AAC fixtures (`.cache/heaac-fixtures/*.m4a`, untracked, generated locally) were
  copied to host a in `~/heaac-fixtures` and staged into the lane by the run command.
  The lane sync deletes untracked directories, so they must be restaged per run.
- Host b has no `~/fixtures/audio`, so the run was pinned to `--host a`.
- `tests/oracle.rs` writes into `std::env::temp_dir()` (i.e. /tmp). I did not change it.

## Left

- Full manual audit of decode-path indexing (see above).
- `sbr_real_library.rs` is over 5000 lines (orchestration limit). It is pre-existing, and
  I did not split it.
- The no-panic test only checks for panics. Checking that each malformed input returns
  `Err` would need a stricter oracle.
