# lane av1clippy3: ec-av1 clippy gate, round 3

## Goal
`cargo clippy -p ec-av1 --all-targets -- -D warnings` exits 0. The 39 errors left by av1clippy2 were all in
`crates/ec-av1/src/encode/` and `crates/ec-av1/src/encoder/` (Frozen: no behaviour change allowed).

## Base
db540eba (main, after the av1clippy2 merge).

## What changed
Mechanical fixes only, all in the 8 files below. No logic changed.

- `encode/more_tests.rs`: `(nw - cw) / 2 & !1` written as `((nw - cw) / 2) & !1` (4 sites, `precedence`);
  two nested `if`/`if let` pairs collapsed into let-chains (`collapsible_if`); a doc-comment
  indent reduced to the list-item rule (`doc_overindented_list_items`).
- `encode/tests.rs`: a blank doc line added before a paragraph that followed a list
  (`doc_lazy_continuation`); an orphan `///` block that was followed by a blank line converted to `//`
  (`empty_line_after_doc_comments`).
- `encode/split.rs`: a stray `#[allow(too_many_arguments)]` left above a comment block with no fn
  removed (`empty_line_after_outer_attr`; `intrabc_enabled` has no args); a duplicate
  `#[allow(too_many_arguments)]` removed (`duplicated_attributes`); an orphan `///` block before
  `//` comments converted to `//`; `((sums + c/2) / c) as u16` in an `if counts > 0` replaced by
  `checked_div(..).unwrap_or(0)` (`manual_checked_ops`, same result).
- `encode/inter_search.rs`: `STAGE_NS` thread-local initializer wrapped in `const { }`
  (`thread_local_initializer_can_be_made_const`).
- `encode/coef.rs`, `encode/frame.rs`, `encode/inter_search.rs`, `encode/split.rs`: nine signatures
  with `too_many_arguments` given a scoped `#[allow]` and a one-line reason comment (spec-shaped
  signature), same style as av1clippy2. `frame.rs` also gets a scoped `needless_range_loop` allow with
  a reason on the `cx` loop.
- `encoder/mod.rs`: unnecessary parentheses around a closure body removed (`unnecessary parens`).
- `encoder/tests.rs`: `let ... else { return None; }` rewritten as `?` (`question_mark`); an orphan
  `///` block before a blank line converted to `//` (`empty_line_after_doc_comments`).

Not changed: `scripts/remote-run.sh` (local edits are none; the lane branch does not touch it).

## Verify
- `scripts/remote-run.sh av1clippy3 --host auto -- bash -c 'cargo clippy -p ec-av1 --all-targets --keep-going -- -D warnings'`
  - baseline run (main): exit 101, 39 errors in `encode/` and `encoder/` (lib test target).
  - after fixes (run on host c): **exit 0**, no warnings or errors.
- Gate, host b, detached (log on host b at `~/av1clippy3-gate.log`):
  `EC_MEMGUARD_MAX=6G cargo test --release -p ec-av1 -- --test-threads=4`
  - lib target: **807 passed, 0 failed, 49 ignored**.
  - second target: 4 passed, 0 failed. Exit code **GATE_RC=0**.
  - Matches av1clippy2's gate counts (807 / 49 / 0).

## Left
Nothing for ec-av1 clippy. Other crates were not checked by this lane.
