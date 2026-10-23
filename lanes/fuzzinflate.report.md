# fuzzinflate: cargo-fuzz targets for ec-inflate

Goal: DONE.md shared gate 2, at least one cargo-fuzz target per untrusted entry point in ec-inflate.
Base sha: 78ec1905 (main at worktree creation).

## What changed
- `crates/ec-inflate/fuzz/`: standalone cargo-fuzz crate (`ec-inflate-fuzz`, own `[workspace]`, path dep on ec-inflate).
  - `inflate_raw`: whole input through `inflate(data, 16 MiB)`.
  - `inflate_zlib`: whole input through `inflate_zlib(data, 16 MiB)`.
  - `inflate_into`: byte 0 bit 0 picks `Format::Raw` or `Format::Zlib`; the output buffer starts as `"prefix"`.
    Asserts the prefix survives and `out.len() <= 16 MiB`, so the limit is checked to count the whole buffer.
  - `inflate_diff`: differential against `miniz_oxide` (`decompress_to_vec_with_limit` / `_zlib_with_limit`,
    same 16 MiB cap). Asserts equal bytes whenever both succeed.
- `miniz_oxide 0.8.9` (already in Cargo.lock via flate2) is a fuzz-crate dependency under `[dependencies]`,
  not `[dev-dependencies]`. Cargo does not expose dev-dependencies to `[[bin]]` targets, so the dev-dependency
  form failed to build. It is confined to the fuzz crate; ec-inflate itself is unchanged.
- `crates/ec-inflate/fuzz/seed.sh`: python3 zlib streams (levels 0, 1, 6, 9) over text, zeros, random and mixed
  inputs, plus the first 8 KiB of six `fixtures/` files. 216 seeds across the four targets. Corpus is git-ignored.
- `crates/ec-inflate/fuzz/.gitignore`: copied from ec-aac/fuzz (`target`, `corpus`, `artifacts`, `coverage`).
- Root `Cargo.toml` already excludes `crates/*/fuzz`, so no change there.

No change to ec-inflate source: no crash, timeout or OOM was found, so no fix or regression test was needed.

## Verify commands and results
All run through `scripts/remote-run.sh fuzzinflate` from the worktree.
- `cargo +nightly fuzz build --fuzz-dir crates/ec-inflate/fuzz`: exit 0 (after moving miniz_oxide to `[dependencies]`;
  the first attempt failed with unresolved `miniz_oxide` in `inflate_diff`).
- `cargo +nightly fuzz run <t> --fuzz-dir crates/ec-inflate/fuzz -- -max_total_time=120 -rss_limit_mb=3000 -timeout=10`
  (`--host a`, which fell back to host b or local on slot contention; all four ran on the server):

| target       | exit | execs in 121 s | crash |
|--------------|------|----------------|-------|
| inflate_raw  | 0    | 2086621        | none  |
| inflate_zlib | 0    | 268264         | none  |
| inflate_into | 0    | 255572         | none  |
| inflate_diff | 0    | 150172         | none  |

- `cargo clippy -p ec-inflate --all-targets -- -D warnings`: exit 0.
- `cargo test -p ec-inflate --release`: exit 0. Unit tests 2 passed; integration tests `tests/inflate.rs` 7 passed.

## Left open
- The gate asks for 1 CPU-hour per target. Only 120 s smoke runs were done, so the gate is not met.
- Differential target: any case where miniz_oxide accepts and ec-inflate rejects (or the reverse) is not a finding by
  design. Only byte mismatches on mutual success are asserted. Disagreement on acceptance is not measured.
- `fuzz/Cargo.lock` was generated on the host and is not in this commit (same as fuzzaudio1).
- `lib.rs` keeps one `.expect("a four byte slice")` on the zlib trailer. It is unreachable because `rest.get(..4)`
  already guarantees length four, so it was not changed.
