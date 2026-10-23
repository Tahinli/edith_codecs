# av1splitp0: test-only source reader for module directories (plan step P0)

Base: `main` @ d271a92b (rebased; the ec-av1 tree is identical between e6274ff1 and d271a92b, and the rebased-in commits touch no crates/ec-av1 file). Plan: `lanes/av1split.plan.md`, sections 1a and Phase 0.

## Goal

The source-scanning tests read files by path with `include_str!("decode.rs")` and friends.
Once `decode.rs`, `stream.rs`, `encode.rs`, `tile.rs`, `qm.rs`, `encoder.rs` and `cdf.rs` become
directories, those includes stop compiling. This step repoints them so a later split changes
no test, and it moves 0 lines of production code.

## What changed

- `crates/ec-av1/src/test_source.rs` (new, `#![cfg(test)]`, declared `#[cfg(test)] mod test_source;`
  in `lib.rs`). Two helpers, both returning `&'static str` (each module is read once per process
  and cached):
  - `module_source(name)`: `src/<name>.rs` if present, plus every `*.rs` under `src/<name>/`
    recursively, in sorted path order. Read at test run time from `CARGO_MANIFEST_DIR/src`.
    Panics if the module has neither form, so a typo cannot read as an empty module.
  - `module_production_source(name)`: the same, minus test-only files (`tests.rs`, `*_tests.rs`).
    Used where a gate scans a module's production body (see below).
- Every `include_str!` of one of the seven split modules is replaced by `module_source(..)`, in
  `gate_coverage.rs`, `stack_budget.rs`, `refusal_inventory.rs`, `decode.rs`, `stream.rs`,
  `encode.rs`. Non-split includes (`cdf_state.rs`, `lib.rs`, `stack_budget.rs`, ...) are unchanged.
- Sites that cut the production prefix at the inline `mod tests` marker now take that prefix from
  `module_production_source`: three `decode_body` sites in `decode.rs`, and `head(..)` in the
  encode chroma gate (`encode.rs`). Without this, after a split the test files would be counted as
  production text and the census would change meaning. For an unsplit file the result is the same.
- `stack_budget.rs`: the `const FILES` table became `fn files()`, since the entries are now read at
  run time. The seven split modules come from the helper, so a new `decode/foo.rs` is scanned
  automatically. `test_source.rs` is listed, which `every_by_value_module_is_scanned` requires
  because `lib.rs` declares it. The local `FILES` in the encoder pin test became a `let`.
- `refusal_inventory.rs`: `const GATE_SOURCES` became `fn gate_sources()`, same reason.
- Two bindings that became unused after the change were removed: `encoder_src` in `encode.rs` and
  `src` in one `decode.rs` gate (clippy caught both).

Production code: no change. Diff is 91 insertions, 83 deletions, plus the new file.

## Verify

Commands run through `scripts/remote-run.sh av1splitp0 -- <cmd>` (every run landed on host c, or a
after host c was busy). Logs are in `~/.cache/av1splitp0-logs/`.

| Check | Baseline (main, unmodified) | After |
|---|---|---|
| `cargo test -p ec-av1 --release --lib -- --list` (count) | 856 tests, 0 benchmarks | 856 tests, 0 benchmarks |
| `cargo test -p ec-av1 --release --lib -- gate_coverage stack_budget` | 28 passed, 0 failed | 28 passed, 0 failed |
| full `cargo test -p ec-av1 --release --lib` | not run on base | 807 passed, 0 failed, 49 ignored (see note) |
| `cargo clippy -p ec-av1 --all-targets` | lib test: 321 warnings, 1 deny-level error (below) | 321 warnings, same per-file counts |
| `cargo check --workspace --all-targets` (5 probes) | not run on base | Finished, exit 0 (probes compile) |

Notes:
- The full suite's first run was killed by the memory guard (`SIGKILL` from
  `memguard-runner.sh` at the default 3500M cap, after 339 tests passed). The rerun used
  `EC_MEMGUARD_MAX=10G -- --test-threads=4` and passed 807 with 0 failed. The 49 ignored tests are
  pre-existing `#[ignore]`s; this diff adds none, and it removes no test.
- Final-tree targeted run after the last two cosmetic edits (unused bindings, indentation):
  `gate_coverage stack_budget refusal_inventory` plus the decode/encode pins by name: 55 passed,
  0 failed.
- Clippy: the baseline already fails `ec-av1 (lib test)` on a deny-level lint at
  `gate_coverage.rs:2645` (`unpinned <= UNPINNED_CEILING`, absurd comparison). That is not from this
  lane and was left alone. Per-file warning counts were compared before and after and are identical.
- `cargo check --workspace --all-targets` shows 14 warnings in lib and probe code. None is in the
  test-only code this step touched; cfg(test) code is not compiled by that command.

## Split proof (scratch, reverted)

In the worktree, uncommitted: `git mv crates/ec-av1/src/qm.rs crates/ec-av1/src/qm/mod.rs`, then
`scripts/remote-run.sh av1splitp0 -- cargo test -p ec-av1 --release --lib -- gate_coverage stack_budget`
gave 28 passed, 0 failed. The rename was then reverted with `git mv` back and the empty directory
removed. Nothing from it is committed.

## Left

- Steps D1 onward (`decode.rs` to `decode/mod.rs` and beyond) are not started.
- The pre-existing clippy deny error in `gate_coverage.rs:2645` still blocks a clean
  `cargo clippy --all-targets -- -D warnings` on ec-av1.
