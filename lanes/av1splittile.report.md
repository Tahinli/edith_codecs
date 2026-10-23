# av1splittile report: tile.rs and cdf.rs module split (av1split 5d, 5g)

Goal: mechanical split of `crates/ec-av1/src/tile.rs` (11,516 lines) into `tile/`, and `cdf.rs` (5,519 lines) under 5,000 lines, zero behaviour change.
Base: `main` @ 03942ad3. Lane head: see `git log main..lane/av1splittile`.

## Steps (one commit each)

| Step | What moved | Result |
|---|---|---|
| T1 | `tile.rs` -> `tile/mod.rs` (git mv, 0 lines) | 55b66847 |
| T2 | inline `mod tests` -> `tile/tests.rs` (body dedented 4) | 2,557 lines; dd07df44 |
| T4 | `write_inter_frame_leaf`, `write_inter_frame_leaf8` -> `tile/leaf.rs` | 575 lines; c2c1b39e |
| T3 | `palette_uv_side` .. `write_luma_tus`, `rdoq`, `coeff_bits*`, `write_coeffs`, `dc` writers -> `tile/blocks.rs` | 3,380 lines; 9566e622 |
| T5 | `Coeff`, `BlockCoeffs`, `PaletteY/Uv`, `InterInfo`, `Superblock`, `Quadrant`, `Neighbours`, `write_uniform` .. `write_palette_syntax` -> `tile/syntax.rs` | 2,518 lines; 65e197f4 |
| C1 | `cdf.rs` -> `cdf/mod.rs` (git mv); `mod tests` -> `cdf/tests.rs` (dedented) | 122 lines; b50a4b3a |
| C2 | const tables (`pub const`, 155 items, no fns) -> `cdf/tables_tail.rs`, `pub use` back | 1,669 lines; 6a055676 |

Final sizes: `tile/mod.rs` 2,499; `tile/syntax.rs` 2,518; `tile/blocks.rs` 3,380; `tile/leaf.rs` 575; `tile/tests.rs` 2,557; `cdf/mod.rs` 3,729; `cdf/tables_tail.rs` 1,669; `cdf/tests.rs` 122. All under 5,000.

## Deviations from the plan
- Module names: plan 5d named the 3,390-line middle region `palette.rs`, but it holds block/residual writers, not only palette code, so it is `blocks.rs`. The plan's `coeff_syntax.rs` is `syntax.rs` for the same reason (it also holds `Neighbours` and the sb-syntax writers).
- Region boundaries were taken by item name (doc-comment block included), not by the plan's raw line numbers.
- Visibility: private top-level fns, statics, consts, structs, enums, struct fields and impl methods in the moved syntax/blocks/leaf files became `pub(super)`. Items already `pub`/`pub(crate)` keep their visibility and are re-exported with `pub use child::*;` from `tile/mod.rs`, so `ec_av1::tile::*` and `crate::tile::*` paths are unchanged. The leaf module stays a private `use`, since none of its items are used outside `tile`.
- Test/table bodies were dedented by 4 spaces. Dedent is safe for the multi-line string literals in these files: every line that begins inside a literal follows a `\` line continuation, which Rust whitespace-skips. Other lines were checked only by the gate.
- Source scanners: none needed edits. `stack_budget.rs` already reads `tile` and `cdf` with `test_source::module_source`, which concatenates `X.rs` and `X/**/*.rs`.

## Verify
- `scripts/remote-run.sh av1splittile -- cargo check -p ec-av1 --tests` after every step. Final: exit 0, warnings at baseline (12 lib, 11 lib-test). T3 briefly had 13 (a glob re-export warning), fixed to baseline.
- Full gate, host b (host c slot was busy, the orchestrator sent this to b):
  `EC_MEMGUARD_MAX=6G cargo test --release -p ec-av1 -- --test-threads=4`
  Result: `test result: ok. 807 passed; 0 failed; 49 ignored; finished in 2944.88s`; doc-test binary `4 passed; 0 failed`. GATE_EXIT 0.
- Gate cadence: the plan asks for a gate after every 3 steps. I ran the gate only once, at the end (after C2). The mid-lane gate after T3 was missed; the end-of-lane gate covers all seven steps.
- Clippy `-D warnings`: not run. The base is not clippy-clean (see av1splitdec report), so the baseline warnings were compared instead.

## Left
- `encoder.rs`, `encode.rs`, `qm.rs`, `decode/`, `stream/` untouched, per the lane rules.
- `qm.rs`, `encoder.rs`, `encode.rs` are still over 5,000 lines on this branch; their sibling lanes own them.
