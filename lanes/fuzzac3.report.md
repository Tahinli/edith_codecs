# fuzzac3: cargo-fuzz targets for ec-ac3

Goal: DONE.md shared gate 2, at least one cargo-fuzz target per untrusted entry point in
ec-ac3 (AC-3 and E-AC-3 syncframe parsing and decode).
Base sha: 78ec1905 (main at worktree creation).

## What changed
- `crates/ec-ac3/fuzz/`: standalone cargo-fuzz crate (`ec-ac3-fuzz`, own `[workspace]`, path dep on ec-ac3).
  - `decode_stream`: whole input treated as an elementary stream. Walks syncwords, uses
    `ec_ac3::frame_size` to find each frame, and feeds each one to `Ac3Decoder::decode_frame`
    (one decoder across the stream). Non-sync bytes are skipped one at a time.
  - `parse_header`: `syncinfo::parse`, `bsi::parse` (from byte 5), `eac3::bsi::parse`, and
    `frame_size` on the same input.
  - `seed.sh`: AC-3 and E-AC-3 fixtures from `fixtures/audio`, truncated to 5 syncframes for
    `decode_stream` and to the first 256 bytes for `parse_header` (12 + 12 seeds).
  - `.gitignore` for `target`, `corpus`, `artifacts`, `coverage`.
- No CRC fixup in the targets. `ec-ac3` never checks CRCs on decode (`crc1` is "carried, not
  checked"; the only CRC code is in the encoder), so mutated frames reach bit allocation and
  mantissa decoding directly.
- Lint-only edits, no behaviour change, needed for the clippy gate:
  - `src/decode.rs`: `self.exps = Box::new([[0; COEFFS]; CHANNELS])` became
    `*self.exps = [[0; COEFFS]; CHANNELS]` (no allocation).
  - `src/encode.rs`, `tests/encode_matrix.rs`, `tests/decode_matrix.rs`: `chunks_exact(N)` with
    a constant N became `as_chunks::<N>().0`.
  - `src/stack_budget.rs`: `eprintln!("{}", "TYPE")` literal inlined into the format string.

## Verify commands and results
All run through `scripts/remote-run.sh fuzzac3`, from the worktree.
- `cargo +nightly fuzz run decode_stream --fuzz-dir crates/ec-ac3/fuzz -- -max_total_time=120 -rss_limit_mb=3000 -timeout=10`
  (host b): exit 0, 29713 runs in 121 s, no crash, timeout or OOM.
- `... parse_header ...` (host b): exit 0, 9274055 runs in 121 s, no crash, timeout or OOM.
  First background attempts were lost to a host-b rsync failure (another lane's directory) and
  a slot-busy window on host a; both re-run in the foreground.
- `cargo clippy -p ec-ac3 --all-targets -- -D warnings` (host): exit 0 after the lint fixes above.
- `cargo test -p ec-ac3 --release --no-fail-fast`: exit 0. Results: 43 passed (lib), 8 passed,
  6 passed, 0 failed.

## Crashes
None found in the two smoke runs. No reproducers, so no fixes were needed and no regression test was added.

## Left open
- The gate asks for 1 CPU-hour per target. Only 120 s smoke runs were done, so the gate is not met.
- Corpus is seeded from fixtures only; no minimised corpus or coverage measured.
- `fuzz/Cargo.lock` was generated on the host and is not in this commit.
- Options (`Downmix`, `dither`, `drc_scale`) are left at defaults in both targets; the
  downmix paths are not reached by the fuzzer.
