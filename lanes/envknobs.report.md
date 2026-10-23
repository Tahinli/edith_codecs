# lane envknobs: test knobs set in-process, no unsafe set_var

Removes every `std::env::set_var` / `remove_var` from ec-aac and ec-vp9 tests (Rust 2024 makes them `unsafe`; the workspace forbids unsafe).

## Approach
- `ec-aac/src/knobs.rs` and `ec-vp9/src/knobs.rs` (`#[doc(hidden)] pub mod knobs`): a thread-local `Cell` override per debug switch, checked before the env var. Setters return a guard that restores the previous value on drop.
- Thread-local because the switches are read on the caller's thread (ec-vp9 tile workers and loop-filter threads never read them), so an override covers only its own test's decode.
- No override set means the reader answers from the env exactly as before. Manual `EC_AAC_*` / `EC_VP9_*` runs are unchanged.
- Sites converted: ec-aac tests (obs_real_content, reelh_per_frame, sbr_real_library chunk_a/b/c), ec-vp9 tests (scratch_lfdump, scratch_sweep).
- Extra fixes found by the run:
  - ec-vp9 `stack_budget`: registered `knobs.rs` in the scan list, added an inventory row for `knobs::Override` (16 bytes), and raised the public-row floor from 3 to 4.
  - ec-aac `chunk_b` `probe_explicit_config_and_wide_lag` now honours `EC_AAC_HEAAC_FIXTURES` like its siblings.

## Counts (server, host a, `EC_REQUIRE_FIXTURES=1 cargo test --release -p ec-aac -p ec-vp9`)

| | suites | passed | failed | ignored | clippy -D warnings |
|---|---|---|---|---|---|
| main (envknobs-base) | 41 | 153 | 0 | 1 | 0 |
| lane (envknobs) | 41 | 153 | 0 | 1 | 0 |

The lane run used `EC_AAC_HEAAC_FIXTURES` pointing at the base lane's heaac fixture directory, because the lane's own `.cache` path was absent on the server.
