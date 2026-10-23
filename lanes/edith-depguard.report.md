# edith lane depguard report

Branch `lane-depguard` (worktree `~/.cache/wt/edith-depguard`), from `codecs-integration` at ff42030.
Commits: 89fe665 (drop oxideav-core), 758ddb1 (dependency guard test). Head: 758ddb1.

## Changes
1. `crates/engine/Cargo.toml`: removed `oxideav-core = "0.1.34"`. Cargo.lock lost the `oxideav-core` package and its `engine` entry only; `bytemuck`, `serde_json` and `thiserror` stay (other users). No `oxideav` package remains in Cargo.lock. The Cargo.toml comments that mention `oxideav-vorbis` / `oxideav-ac3` (engine lines ~91, 131, 138) describe the crates that the ec-* crates replaced; they are still true, so they were left alone. Root Cargo.toml's vendor history comment is likewise accurate.
2. `crates/engine/tests/dependency_guard.rs` (new): `no_forbidden_crate_is_in_the_dependency_graph`.
   - Reads the workspace Cargo.lock (no cargo metadata; see deviation below).
   - Walks from `engine`, `engine-hw`, `engine-audio`.
   - Fails, naming the crate and the path, for `symphonia-codec-aac`, `mp4`, `cros-codecs`, `cros-libva`, `gbm`, `image`, `ravif`, `flacenc`, `hound`, `miniz_oxide`, or any `oxideav-*`.
   - `PENDING_WAVE2 = [cros-codecs, cros-libva, gbm]` is tolerated while reachable; the test fails if any of them is no longer reachable.
   - `rav1e`, `symphonia`, `symphonia-core` may be reached only from `engine` directly, or from another of these three (symphonia's internal crates depend on symphonia-core). Any other route fails with the path.
3. `vendor/` holds exactly cros-codecs, cros-libva, proc-macro-error2. The three `[patch.crates-io]` entries match; no stale entry (no mp4 patch existed). Nothing removed.

## Deviation
- The test does not call `cargo metadata`. On the lane server, `cargo metadata --offline` (and `cargo tree`) fail with "failed to download annotate-snippets v0.11.5" because that package is not in the server cache, even with a platform filter. Parsing Cargo.lock avoids this. The lock does not record dependency kinds, so the walk also follows build edges. That is stricter than `cargo tree -e normal`; it is exact today: none of the three roots declares a dev or build dependency.

## Verification
- Guard, passing: `cargo test -p engine --release --test dependency_guard` ok (server).
- Guard proof 1: temporarily added `hound = "3.5"` to engine. Failure: `hound is in the dependency graph: engine -> hound`. Reverted.
- Guard proof 2: temporarily added `flacenc` to PENDING_WAVE2. Failure: `flacenc is no longer in the dependency graph: delete it from PENDING_WAVE2`. Reverted.
- `cargo test -p engine --release --no-fail-fast` (server): 878 passed, 1 failed, the known `playback::import_refuses_what_does_not_match`. The other allowed failures did not occur. The new guard test passes.
- `cargo build -p edith --release` (server): ok.
- `cargo clippy -p engine --release --all-targets` (server): the repo already has ~165 warnings; none point at `dependency_guard.rs`, and the Cargo.toml change adds none. Not run with `-D warnings`, because the baseline is not clean.
- `rustfmt --check` on the new test file: clean.
