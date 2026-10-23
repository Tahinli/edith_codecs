# fuzzvorbis: cargo-fuzz targets for ec-vorbis

Goal: DONE.md shared gate 2, at least one cargo-fuzz target per untrusted entry point in
ec-vorbis (header parsing and audio packet decode).
Base sha: 78ec1905 (main at worktree creation).

## What changed
- `crates/ec-vorbis/fuzz/`: standalone cargo-fuzz crate (`ec-vorbis-fuzz`, own `[workspace]`,
  path dep on ec-vorbis). Root `Cargo.toml` already excludes `crates/*/fuzz`.
  - `setup`: three u16-BE-length-prefixed packets (identification, comment, setup). The
    identification and comment parsers run on their own, then all three go through
    `VorbisDecoder::new`, which runs `Setup::parse` (codebooks, floors, residues, mappings, modes).
  - `decode`: byte 0 selects one of three header sets (mono, stereo, 5.1 at 48 kHz) embedded with
    `include_bytes!` from `fuzz/headers/*.bin`; then u16-BE-length-prefixed audio packets decoded in
    sequence on one `VorbisDecoder`.
  - `seed.sh`: writes `headers/*.bin` (one fixture per layout, committed) and the corpus under
    `fuzz/corpus/` (git-ignored): setup seeds are the headers of all 31 Ogg Vorbis files, decode
    seeds are selector + up to 200 audio packets (64 KiB cap) for each file with 1, 2 or 6 channels.
    Ogg pages are parsed in python3 (no ec-ogg example needed).
  - `.gitignore`: same as the other fuzz crates.
- `crates/ec-vorbis/src/codebook.rs`, `setup.rs`: **OOM fix** (see Crashes). `Codebook::parse` now
  takes a per-header `value_budget`; a lookup table of `entries * dimensions` values is charged
  against it before allocation. `Setup::parse` gives each setup header `CODEBOOK_VALUE_BUDGET = 1 << 22`
  (4M floats, 16 MiB). Regression test `a_lookup_table_past_the_header_budget_is_refused`.
- `crates/ec-vorbis/src/decode.rs`, `encode.rs`, `tests/oracle.rs`: clippy-only changes, no behaviour
  change. `u64::from(x as u64)` simplified; `chunks_exact(4)` replaced by `as_chunks::<4>()`; a
  `#[allow(clippy::too_many_arguments)]` with a reason on `fit_floor`; the oracle test's `push_str("\n")`,
  complex return types (type alias `ResidueCapture`), a `vec![with_capacity; n]`, an index loop and an
  unused helper `decode_capture` (no callers) were tidied. No `#[ignore]` added or removed.

## Verify commands and results
All cargo commands ran through `scripts/remote-run.sh fuzzvorbis`, from the worktree. `--host a` was
busy for the whole 10-minute retry window on the first attempt (exit 75), so later runs used `--host auto`.
Host a and b were both slot-busy for about 10 minutes once and one run fell back to local; that local
attempt failed at once (its manifest was not yet written) and ran nothing.

- `cargo +nightly fuzz build --fuzz-dir crates/ec-vorbis/fuzz`: exit 0 (both targets built).
- `cargo +nightly fuzz run setup --fuzz-dir crates/ec-vorbis/fuzz -- -max_total_time=120 -rss_limit_mb=3000 -timeout=10`:
  exit 0, 44479 runs in 121 s (first run), 36968 runs in 121 s (after the fix). No crash, no timeout.
  Peak RSS reported 490 MB.
- `cargo +nightly fuzz run decode --fuzz-dir ... (same flags)`: exit 0, 2384 runs in 121 s (first run),
  2578 runs in 121 s (after the fix). No crash, no timeout. Peak RSS reported 532 MB.
  The RSS is most likely cargo-fuzz's default AddressSanitizer build; not measured without it.
- `cargo clippy -p ec-vorbis --all-targets -- -D warnings`: first run exit 101 (3 errors in src, 9 in
  tests/oracle.rs). After the fixes above: exit 0.
- `cargo test -p ec-vorbis --release`: exit 0. Lib: 14 passed, 0 failed. Oracle: 8 passed, 3 ignored
  (the 3 ignores were already there), 0 failed.

## Crashes, timeouts, OOM
- No libFuzzer crash, timeout or OOM in any of the four 120 s smoke runs. Nothing was minimised.
- **Found by code reading, not by the fuzzer (1 distinct issue, 1 fix attempt, succeeded):**
  `Codebook::parse` bounded `entries` against the bits left in the header, but not the expanded
  table. A type 1 or 2 lookup allocates `entries * dimensions` floats (up to 2^24 per book, which is
  64 MiB), and the header only has to state one multiplicand. A setup header of about 45 KB, with 256
  books of 256 entries and 65535 dimensions, would allocate about 16 GiB. The fix is the per-header budget
  above. Not reproduced by running the target; the regression test is a unit test of the same header.
  Note: the regression test was written against the fixed signature, so I did not run it on the old
  code to see it fail.

## Left open
- The gate asks for 1 CPU-hour per target. Only 120 s smoke runs were done, so the gate is not met.
- Corpus is seeded from fixtures only; no minimisation, no coverage measured.
- `fuzz/Cargo.lock` is generated on the host and is not in this commit (same as fuzzaudio1).
- Huffman tree node count (`assign_codes`) is bounded by header bits times the codeword length, not
  by a budget. Not measured; a crafted header could make it large. Worth a follow-up.
- Exec counts are low for `decode` (about 20 runs/s) because each run builds a decoder from the full
  header set; a longer run would need a cheaper harness or a persistent decoder per header set.
