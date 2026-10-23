# lane opusclippy2: ec-opus all-targets clippy clean

## Goal
`cargo clippy -p ec-opus --all-targets -- -D warnings` clean. The lib was already clean on main;
18 errors remained in `crates/ec-opus/tests/conformance.rs`.

## Base
main @ 131d3a0c

## What changed
Only `crates/ec-opus/tests/conformance.rs` (18 lines). All 18 diagnostics were
`clippy::chunks_exact_to_as_chunks` ("using `chunks_exact` with a constant chunk size"):

- `x.chunks_exact(N)` with a literal or const N, used as an iterator chain, became
  `x.as_chunks::<N>().0.iter()`.
- `for block in x.chunks_exact(EXPR)` became `for block in x.as_chunks::<{ EXPR }>().0`.
- Line 6127 has `.take(to)` chained after the iterator, so it became `.as_chunks::<{ 960 * CH }>().0.iter().take(to)`.

Semantics are unchanged. `as_chunks` yields the same full chunks as `chunks_exact` and drops the same
remainder. Each block is only passed on as a slice, so the array-ref items coerce cleanly. Non-constant
`chunks_exact(channels)` / `chunks_exact(960 * channels)` calls were not flagged and were left alone.
No test was edited, ignored, or removed. No `#[allow]` was needed.

## Verify
- `scripts/remote-run.sh opusclippy2 -- cargo clippy -p ec-opus --all-targets -- -D warnings`: exit 0.
- `scripts/remote-run.sh opusclippy2 -- cargo test -p ec-opus --release`, second run (exit 0):
  - lib: 42 passed, 0 failed
  - conformance: 28 passed, 0 failed, 16 ignored
  - malformed_input: 3 passed, 0 failed
- First test run (exit 101): `conformance::decode_speed` failed with
  "opus-ogg-5.1-48000.opus: 104x realtime is below the 104x floor". This is a wall-clock throughput
  floor on a shared server. The test only runs the decoder, which this change does not touch, and the
  identical code passed on the rerun. Treated as timing noise, not a regression. The floor is tight
  (104x against a 104x threshold) and could flake again under load.

## Left
- The decode_speed 5.1 floor is marginal on the server. Raising load or CPU contention can trip it. It is not
  a clippy item and was not changed.
