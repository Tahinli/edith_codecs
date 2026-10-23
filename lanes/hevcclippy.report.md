# lane hevcclippy: ec-h265 clippy clean

## Goal
`cargo clippy -p ec-h265 -p ec-h265-syntax --all-targets -- -D warnings` exits 0, with zero
behaviour change. The RDOQ numerics stay bit-identical.

## Base
`15931054` (main, merge of lane/hevcdecapi).

## What changed
- `crates/ec-h265/src/residual.rs`
  - `lambda * bits as f64` (and `trial_bits`, `zero_bits`) became `lambda * bits`. The operands
    are already `f64` (`levels_bits` returns `f64`), so the cast was a no-op. Six sites, lib and
    test module.
  - `if let Some(skip) = transform_skip { if log2_size == 2 {..} }` became a let chain. Same
    short-circuit, same bits.
  - `if i == last_sub { true } else if i == 0 { true } else {..}` became
    `if i == last_sub || i == 0 { true } else {..}`.
  - `sig_ctx` was declared late and assigned in each branch. It is now one `let sig_ctx = if ..`
    expression. The `base.expect(..)` path returns early as before, with the same operands.
- `crates/ec-h265/src/transform.rs`: `row % 2 == 0` became `row.is_multiple_of(2)` in `m32`
  (unsigned, same value).
- `crates/ec-h265/src/decode.rs`: `#[allow(clippy::too_many_arguments)]` with a reason comment on
  `reconstruct_luma` (8 inputs), `reconstruct_chroma` (9) and `decode_single_tu` (8). A parameter
  struct would only move the arguments. No logic changed.

No float expression was reordered, no integer type changed, and no test was touched or ignored.

## Verify
Run on the server through `scripts/remote-run.sh hevcclippy`.

- `cargo clippy -p ec-h265 -p ec-h265-syntax --all-targets -- -D warnings`: **exit 0**, no
  warnings or errors. Before the change: 6 errors in lib and 3 more in the test target
  (`residual.rs`), all in the `as f64` / collapsible-if / identical-blocks / late-init group.
- `cargo test -p ec-h265 -p ec-h265-syntax --release`: **all pass, 0 failed**.
  - ec-h265 lib 30, conformance 15, dec_shell 13, decode 5, encode 4, perf 3, rdoq_cache_hash 3.
  - ec-h265-syntax unit 14, doc 2, fuzz 0.
  - `rdoq_cache_hash` (3/3) pins the encoder output hashes and passes unchanged.

## What is left
Nothing for this lane. The clippy target is met.
