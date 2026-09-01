# lane-covbd r1 — gate coverage per (tool x bit depth)

## What changed
- `crates/ec-av1/src/gate_coverage.rs`
  - `TOOL_UNIVERSE` (26 `--enable-*` flags, fixed in source, not derived): the flags the
    decoder cares about, so a flag *no* gate spells still counts as a hole (defaulted =
    unknown, the pre-existing rule).
  - `NEVER_EXERCISED_8BIT` (12 entries) / `NEVER_EXERCISED_10BIT` (19 entries), each
    `(flag, why)`.
  - tests `never_exercised_8bit_matches_the_gate_recipes` /
    `never_exercised_10bit_matches_the_gate_recipes`: negative check (every listed entry
    really has no `=1` at that depth) and positive check (a flag that a gate of that depth
    now enables must be deleted from the list — the test fails until it is).
  - `is_ten_bit(body)`: a gate is 10-bit if its recipe contains `encode_10bit_gradients`,
    `--bit-depth=10`, `--input-bit-depth=10`, or a `yuv420p10le` fixture (that is how the
    existing 10-bit gates spell it, stream.rs:1905/1934/2141/9001).
  - `gate_bodies()` / `flags_in()` factored out of `tool_settings()`; the original
    `NEVER_EXERCISED` list and its test are untouched.
  - `print_never_exercised_per_bit_depth`: the orchestrator helper.
- No gate recipe touched; `stream.rs` unchanged.

## Finding
45 real-aomenc gates, **4 of them 10-bit**. Seven tools are positively enabled at 8 bits and
by no 10-bit gate: `restoration`, `cdef`-adjacent intra set (`filter-intra`,
`intra-edge-filter`, `paeth-intra`, `smooth-intra`), `rect-partitions`, `ab-partitions`.
`enable-restoration` is the instance that cost two hbd defects (SGR box sums unscaled,
Wiener clamp at the 8-bit bound, lane-hbdinter 2026-09-01).

Caveat recorded in the list itself: `enable-superres` is a *flag-spelling* hole, not a real
one — the three superres gates drive `--superres-mode=1` (one of them 10-bit).

## Printed lists (verbatim)
```
gate_coverage: 45 real-aomenc gates, 4 of them 10-bit
NEVER_EXERCISED_8BIT (12 of 26):
    --enable-1to4-partitions
    --enable-angle-delta
    --enable-cfl-intra
    --enable-dist-wtd-comp
    --enable-dual-filter
    --enable-flip-idtx
    --enable-global-motion
    --enable-intrabc
    --enable-rect-tx
    --enable-ref-frame-mvs
    --enable-superres
    --enable-tx64
NEVER_EXERCISED_10BIT (19 of 26):
    --enable-1to4-partitions
    --enable-ab-partitions
    --enable-angle-delta
    --enable-cfl-intra
    --enable-dist-wtd-comp
    --enable-dual-filter
    --enable-filter-intra
    --enable-flip-idtx
    --enable-global-motion
    --enable-intra-edge-filter
    --enable-intrabc
    --enable-paeth-intra
    --enable-rect-partitions
    --enable-rect-tx
    --enable-ref-frame-mvs
    --enable-restoration
    --enable-smooth-intra
    --enable-superres
    --enable-tx64
```

## Gates / tests
- `cargo test -p ec-av1 --lib gate_coverage -- --nocapture` -> 5 passed, 0 failed.
- `cargo test -p ec-av1 --lib refusal_inventory` -> 3 passed, 0 failed.
- `cargo check -p ec-av1` -> clean (118 pre-existing missing-doc warnings).
- EVIDENCE: crates/ec-av1/src/gate_coverage.rs | cargo test -p ec-av1 --lib gate_coverage -- --nocapture | 5/5 pass, 45 gates parsed, 4 ten-bit, 12/19 holes printed above

## Refusals lifted
None — this lane is instrumentation only, no decode behaviour changed.

## Residue
- accepted: `enable-superres` and `enable-cdef`-style aliasing. Tools driven by a non
  `--enable-*` flag (`--superres-mode=1`) read as holes. Fix is an alias map, one line per
  alias, when a second case appears.
- deferred: closing any 10-bit hole — other lanes own gate authoring; this lane was
  chartered not to write gates. What unblocks it: a 10-bit gate passing `=1` for the tool.
- accepted: `cargo fmt -p ec-av1` reformatted 18 unrelated files; reverted, only
  gate_coverage.rs is in the commit.
