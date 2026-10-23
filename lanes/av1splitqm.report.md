# av1splitqm: qm.rs module split (av1split section 5e, steps Q1-Q4)

Goal: mechanical split of `crates/ec-av1/src/qm.rs` (7,046 lines) into `qm/` so no file is over
5,000 lines, with zero behaviour change.
Base: `main` @ 03942ad3.

## Steps
| Step | What moved | File |
|---|---|---|
| Q1 | `qm.rs` -> `qm/mod.rs` (git mv); `mod tests` body dedented into a child file | `qm/mod.rs`, `qm/tests.rs` |
| Q2 | `static IWT` levels 0-4 -> `pub(super) const IWT_L0..IWT_L4` | `qm/iwt_a.rs` |
| Q3 | levels 5-9 -> `IWT_L5..IWT_L9` | `qm/iwt_b.rs` |
| Q4 | levels 10-14 -> `IWT_L10..IWT_L14`; `static IWT` in mod.rs is now 15 references | `qm/iwt_c.rs` |

Scanner plumbing: none needed. `test_source::module_source("qm")` already reads `src/qm/` as a
directory, and `stack_budget.rs` lists it as `qm.rs` with that call.

## Line counts
| File | Lines |
|---|---:|
| qm/mod.rs | 165 |
| qm/iwt_a.rs | 2278 |
| qm/iwt_b.rs | 2278 |
| qm/iwt_c.rs | 2278 |
| qm/tests.rs | 106 |

## Verification
- Table data: the 100,320 numbers in main's `static IWT` (15 x 2 x 3344) match the concatenation
  of `iwt_a/b/c.rs` in order, with type annotations and comments ignored.
- `cargo check -p ec-av1 --tests` on host c after each step: clean. Warnings are main's (12 lib,
  11 lib-test).
- Test count: 5 `#[test]` attributes in qm before and after.
- Full gate on host c, log `~/lanes/av1splitqm/gate.log`:
  `EC_MEMGUARD_MAX=6G cargo test --release -p ec-av1 -- --test-threads=4`
  -> `test result: ok. 807 passed; 0 failed; 49 ignored` (lib, 3237s), plus `4 passed; 0 failed`
  for the second binary. `GATE_EXIT=0`.
- Not run: clippy. Main is not `-D warnings` clean on this crate, so that gate cannot pass as-is.

## Left
- Other over-5k files are out of scope for this lane: stream.rs (done by its lane), decode.rs (done),
  encode.rs, tile.rs, encoder.rs, cdf.rs (sibling lanes).
