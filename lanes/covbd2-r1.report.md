# lane-covbd2 r1 — default-on tools and non-`--enable-*` spellings

Branch `lane-covbd2`, rebased onto main 593de22 (charter said 06d856d; `git rebase main`
per COMMON, clean, gate counts moved 45/4 -> 56/15).

## What changed (crates/ec-av1/src/gate_coverage.rs only; no gate recipe touched)
- `On` enum + `DEFAULT_ON_TOOLS` (10 entries, each `(tool, aomenc spelling(s), aomenc default,
  what counts as on)`) — the third instance of class *tool-disabled-in-every-gate*: lane-covbd's
  derivation read `"--enable-` only, so `--enable-tx-size-search=0` (34/41 8-bit gates),
  `--loopfilter-control=0`, `--tile-columns/--tile-rows` and `--cpu-used` were invisible.
- `ALIASES` (`superres-mode` -> `enable-superres`), applied in `enabled_at()`: the three
  superres gates drive `--superres-mode=1`, so `enable-superres` is now *covered* and its two
  entries were deleted from `NEVER_EXERCISED_8BIT`/`_10BIT` (the covbd deferral, closed).
- `NEVER_ON_8BIT` (6) / `NEVER_ON_10BIT` (7), each `(tool, why)`, with the off/defaulted split in
  the reason. Same "defaulted means unknown" rule: a default of `1` is not proof the encoder
  picked the tool, only an explicit on-value at that depth retires an entry.
- `settings_in()` (whole `--flag=value` pairs, values kept whole — `--cpu-used=4`,
  `--tile-columns=2`), `default_on_state()`, `default_on_settings()`, `never_on_at()`.
- Tests: `never_on_8bit_matches_the_gate_recipes`, `never_on_10bit_matches_the_gate_recipes`
  (both directions: unlisted hole fails, closed hole fails until deleted),
  `default_on_tools_do_not_duplicate_the_universe` (no double-pinning, every alias names a real
  `TOOL_UNIVERSE` tool), `print_never_on_per_bit_depth` (helper).

## Printed lists (verbatim)
```
gate_coverage: 56 real-aomenc gates, 15 of them 10-bit
NEVER_EXERCISED_8BIT (11 of 26):
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
    --enable-tx64
NEVER_EXERCISED_10BIT (9 of 26):
    --enable-1to4-partitions
    --enable-angle-delta
    --enable-cfl-intra
    --enable-dual-filter
    --enable-flip-idtx
    --enable-intrabc
    --enable-rect-tx
    --enable-ref-frame-mvs
    --enable-tx64
NEVER_ON_8BIT (6 of 10, over 41 8BIT gates):
    deblocking (--loopfilter-control): off in 7, defaulted in 34, on in 0
    enable-diff-wtd-comp (--enable-diff-wtd-comp): off in 6, defaulted in 35, on in 0
    enable-directional-intra (--enable-directional-intra): off in 32, defaulted in 9, on in 0
    enable-fwd-kf (--enable-fwd-kf): off in 11, defaulted in 30, on in 0
    enable-onesided-comp (--enable-onesided-comp): off in 17, defaulted in 24, on in 0
    enable-tx-size-search (--enable-tx-size-search): off in 34, defaulted in 7, on in 0
NEVER_ON_10BIT (7 of 10, over 15 10BIT gates):
    deblocking (--loopfilter-control): off in 1, defaulted in 14, on in 0
    enable-diff-wtd-comp (--enable-diff-wtd-comp): off in 1, defaulted in 14, on in 0
    enable-fwd-kf (--enable-fwd-kf): off in 3, defaulted in 12, on in 0
    enable-interintra-wedge (--enable-interintra-wedge): off in 5, defaulted in 10, on in 0
    enable-onesided-comp (--enable-onesided-comp): off in 5, defaulted in 10, on in 0
    enable-tx-size-search (--enable-tx-size-search): off in 11, defaulted in 4, on in 0
    multi-tile (--tile-columns,tile-rows): off in 0, defaulted in 15, on in 0
```
Two entries of `DEFAULT_ON_TOOLS` are NOT holes and so print nowhere:
`enable-smooth-interintra` and `intrabc-search` (`--cpu-used<=2`, on in 51 gates) are
positively exercised at both depths; `multi-tile` is exercised at 8 bits only
(`--tile-columns=1/2`, `--tile-rows=1/2`).

## Findings worth a lane
- `enable-tx-size-search` pinned off in 34/41 8-bit and 11/15 10-bit gates, on in none — the pin
  that hid the real-stream PANIC lane-ab16 hit; lane-txselect landed TX_MODE_SELECT but no gate
  spells the flag on, so the guard now says so.
- `multi-tile` at 10 bits: every one of the 15 10-bit gates is single-tile, so no tile edge or
  per-tile CDF reset is checked at high bit depth (the `enable-restoration` shape that cost
  lane-hbdinter two 10-bit-only defects).
- `deblocking`: no gate at either depth spells `--loopfilter-control=1`; 7+1 pin it to 0.

## Tests
- `cargo test -p ec-av1 --lib gate_coverage -- --nocapture` -> 9 passed, 0 failed (was 5).
- `cargo test -p ec-av1 --lib refusal_inventory` -> 3 passed, 0 failed.
- EVIDENCE: crates/ec-av1/src/gate_coverage.rs | cargo test -p ec-av1 --lib gate_coverage -- --nocapture (CARGO_TARGET_DIR=~/.cache/cargo-target-covbd2) | 9/9 pass, 56 gates parsed, 15 ten-bit, lists above

## Refusals lifted
None — instrumentation only, no decode behaviour changed.

## Residue
- deferred: closing any NEVER_ON hole — chartered "do NOT write gates". What unblocks it: a gate
  at that depth spelling the on-value and asserting the feature fired; the test then fails until
  the entry is deleted.
- accepted: `--cpu-used` is modelled as one tool (`intrabc-search`, on at <=2). If another
  cpu-used-gated search needs its own threshold, add a second entry with its own `On::AtMost`.
- accepted: no repo-wide `cargo fmt`; only gate_coverage.rs is in the commit.
