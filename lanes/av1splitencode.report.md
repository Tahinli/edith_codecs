# av1splitencode: encode.rs and encoder.rs module split (av1split 5c, 5f)

Goal: mechanical split of `crates/ec-av1/src/encode.rs` (23,771 lines) and `crates/ec-av1/src/encoder.rs`
(6,503 lines) so that no file under `crates/ec-av1/src/encode*` is over 5,000 lines, with no behaviour change.

Branch: `lane/av1splitencode`. Base: `main` (merged as the lane's merge commit `c0c546b1`, which brought in
the qm/, tile/ and cdf/ splits; those touch other files only).

## What changed

| Step | What moved | File |
|---|---|---|
| E1 | `encode.rs` -> `encode/mod.rs` (git mv, 0 lines) | `encode/mod.rs` |
| E2 | `mod tests` (second half) | `encode/more_tests.rs` |
| E3 | `mod tests` (first half) | `encode/tests.rs` |
| E4 | TPL, deltaq, `encode_inter_frame`, `EncodedSequence` | `encode/frame.rs` |
| E5 | `record_mi*`, `Search`, `Trial`, `search_inter_block` | `encode/inter_search.rs` |
| E6 | `SPLIT_CENSUS`, `split_census*`, `code_square_inter`, Ibc | `encode/split.rs` |
| E7 | `CoefCtx`, `Plane`, `At`, `Reach`, tx tables | `encode/coef.rs` |
| N1 | `encoder.rs` -> `encoder/mod.rs` (git mv); inline `mod tests` -> `encoder/tests.rs` | `encoder/mod.rs`, `encoder/tests.rs` |
| E8 | check only: `encode/mod.rs` under 5,000 | |
| merge | `git merge main` (clean, no conflicts) | |

Source-scanning tests reach the encode and encoder sources through `test_source::module_source("encode")` and
`module_source("encoder")` (directory-aware), so the moved files stay visible to them. No `include_str!`
path still names `encode.rs` or `encoder.rs`.

## Line counts (`crates/ec-av1/src/encode/` and `crates/ec-av1/src/encoder/`)

| File | Lines |
|---|---:|
| `encode/inter_search.rs` | 4,671 |
| `encode/tests.rs` | 3,975 |
| `encode/more_tests.rs` | 3,935 |
| `encode/split.rs` | 3,334 |
| `encoder/tests.rs` | 3,943 |
| `encode/frame.rs` | 2,867 |
| `encoder/mod.rs` | 2,559 |
| `encode/mod.rs` | 2,572 |
| `encode/coef.rs` | 2,443 |

Largest: `encode/inter_search.rs` at 4,671. Nothing under `encode*` is over 5,000 lines.
The original files were 23,771 (`encode.rs`) and 6,503 (`encoder.rs`) lines.

## Verify

- `scripts/remote-run.sh av1splitencode -- cargo check -p ec-av1 --tests` (host c): exit 0, `Finished`.
  Warnings: `ec-av1` lib 12, lib test 11, the same counts main has.
- Full gate on host b:
  `EC_MEMGUARD_MAX=6G cargo test --release -p ec-av1 -- --test-threads=4`
  Result: `test result: ok. 807 passed; 0 failed; 49 ignored; 0 measured; finished in 3002.14s`,
  plus `4 passed; 0 failed` (second binary) and doc tests `0 passed`. Exit 0.

## Notes

- The E2/E3 test halves are named `more_tests.rs` and `tests.rs` on this branch (the plan's `tests_b.rs` name was not used).
- No test was removed, weakened or `#[ignore]`d by this lane (moves only).

## Left

- Nothing in the `encode*` scope. Remaining plan sections 5d (tile, now on main), 5e (qm, now on main)
  and 5g (cdf, now on main) are outside this lane.
