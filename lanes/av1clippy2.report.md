# lane av1clippy2: ec-av1 clippy gate, round 2

## Goal
`cargo clippy -p ec-av1 --all-targets -- -D warnings` toward clean (DONE.md gate 1). Not fully clean:
39 errors remain, all in `crates/ec-av1/src/encode/` and `crates/ec-av1/src/encoder/`, which this lane
did not touch (the sibling split lane owns them).

## Base
bdc74c55 (main at that point). Merged main a00c9362 / daf9ba1e (includes the encode/ and encoder/ split)
before the final verify.

## What changed (commits on lane/av1clippy2)
1. `1ef45907` machine-applicable fixes from `cargo clippy --fix` run on host c: unused `mut`, needless
   parens, `needless_borrow`, `chunks_exact` -> `as_chunks`, `manual_div_ceil`, `manual_is_multiple_of`.
   `encoder.rs` was reverted, since it is out of scope.
2. `0f0af13e` applied clippy's own suggestions from the JSON diagnostics (collapsible_if, precedence,
   needless_lifetimes, implicit_saturating_sub, identity_op, filter_next, collapsible_match,
   useless_format, manual_clamp, items_after_test_module, and others). `items_after_test_module` moved
   a test-only block in `speed.rs`; this does not change behaviour.
3. `385f8e20` doc continuation indents (`doc_lazy_continuation`), empty lines after doc comments,
   `checked_div` for `manual_checked_ops` in `decode/deblock.rs`, `pin_reporting` in `dumpio.rs` made
   `#[cfg(test)]` (its only caller is the self-test; it was dead in the non-test build), duplicate
   `#[allow]` removed, `as_chunks` closure fix in `tile/tests.rs` (the machine suggestion did not compile).
4. `e6e92bfe` redundant `if` branches merged (`if_same_then_else`), `explicit_counter_loop` rewritten
   with `(start..).take(n)`, `let cdfs = cdfs;` removed (`redundant_locals`), `#[must_use]` removed from
   the guard-returning fns in `speed.rs` (`double_must_use`), `as_chunks` in `film_grain.rs`.
5. `4a49df60` scoped allows, each with a one-line reason comment:
   - `clippy::too_many_arguments` on the signature (per fn): spec-shaped kernel signatures.
   - `clippy::needless_range_loop` on the loop (per statement): index loops that mirror the spec.
   - `clippy::type_complexity` crate-level in `lib.rs`: tuple-of-slice signatures are pervasive.
   - `clippy::large_enum_variant` on `tile/syntax.rs::Superblock`: boxing would allocate per superblock.
   - `clippy::absurd_extreme_comparisons` on a `let` in `gate_coverage.rs`: the ceiling is a deliberate 0.
6. Merge of main into the lane branch (no conflicts; the sibling's `remote-run.sh` local edit in this
   worktree was saved aside and not committed).

## Lint counts (ec-av1, all targets, `-D warnings`)
Before = baseline bdc74c55, lib test target only (clippy stopped there, 315 errors). Examples and
integration tests were not reached at baseline. After = merged tree, all targets, 39 errors.

| lint | before | after (encode/encoder only) |
|---|---|---|
| too_many_arguments | 62 | 9 |
| type_complexity | 60 | 0 (crate-level allow) |
| needless_range_loop | 47 | 1 |
| doc_lazy_continuation | 33 | 9 |
| collapsible_if | 21 | 2 |
| precedence | 9 | 8 |
| chunks_exact_to_as_chunks | 8 | 0 |
| empty_line_after_doc_comments | 6 | 3 |
| needless_borrow | 6 | 0 |
| manual_div_ceil | 5 | 0 |
| manual_is_multiple_of | 4 | 0 |
| manual_checked_ops | 3 | 1 |
| other (about 25 lints, 1-2 each) | ~53 | 6 |
| **total** | **315** | **39** |

The after counts are all in `encode/` and `encoder/`, which the sibling lane is splitting. The lint
mix there has moved because the code moved. Outside those paths there are 0 errors. This was checked
on all targets with the two encode modules masked by `#![allow(clippy::all, unused_parens)]` on the host
copy only (never committed). Pre-merge this gave 0 diagnostics, and post-merge it also gave 0.

## Verify
- `scripts/remote-run.sh av1clippy2 --host c -- cargo clippy -p ec-av1 --all-targets --keep-going -- -D warnings`:
  FAIL, 39 errors, all in `encode/` and `encoder/`.
- Same command with encode modules masked on the host copy: PASS, 0 diagnostics on lib, lib test,
  examples, tests.
- `EC_MEMGUARD_MAX=6G cargo test --release -p ec-av1 -- --test-threads=4` on host b, merged tree:
  **807 passed, 49 ignored, 0 failed**, GATE_RC=0.

## Left
- 39 clippy errors in `encode/` and `encoder/` (out of scope for this lane): too_many_arguments 9,
  doc_lazy_continuation 9, precedence 8, empty_line_after_doc_comments 3, collapsible_if 2, and one
  each of needless_range_loop, manual_checked_ops, duplicated_attributes, unused_parens,
  missing_const_for_thread_local, doc_overindented_list_items, question_mark,
  empty_line_after_outer_attr.
- Clippy cannot pass the gate until those files are fixed. The same fixes (per-site allows or
  suggestion application) apply there.
