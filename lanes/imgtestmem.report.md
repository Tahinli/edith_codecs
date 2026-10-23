# imgtestmem: ec-image tests under the default memory cap

Goal: `cargo test -p ec-image --release` (default threads) exits 0 under the default
`EC_MEMGUARD_MAX=3500M` cap on the lane servers.
Base sha: 1881f05a (main at worktree creation).

## What changed
- `crates/ec-image/src/lib.rs`: `decode_animation_with_limits(data, limits)`. `decode_animation`
  calls it with `Limits::default()`.
- `crates/ec-image/tests/fuzz.rs`:
  - `HEAVY` mutex with `heavy()` (poison-recovering). Held by every mutation sweep and by the
    budget-sized tests, so at most one runs at a time. Cheap tests stay parallel.
  - `budget_limits()`: 1 GiB total, 512 MiB per buffer. The sweeps, the budget tests, the
    few-frame GIF test, the LZW bomb and the arbitrary-bytes test decode under it.
  - Budget tests use 4000x2000 canvases. A canvas that divides 1 GiB evenly lets the last clone
    land exactly on the budget, and the test's own allocations then push it over.
  - New `decode_animation_applies_the_default_limits`: plain and explicit-default paths agree, and
    a one-byte total budget refuses the same file.
- `crates/ec-image/tests/fuzz_regressions.rs`: the PNG, BMP and JPEG budget tests take `HEAVY`.
- No test deleted, ignored or weakened. No assertion changed. `Limits::default()` and the cap are
  unchanged.

## Why the default-limit canvas case is not tested
A GIF canvas over the default `max_alloc` cannot be built: `max_pixels` (2^28) caps a canvas at
exactly `max_alloc` (2^30 bytes). The new test covers the default path a different way.

## Measured peaks (host a, unguarded, one test at a time)
- Budget tests (GIF and WebP many-frame): about 1010 MiB each.
- Sweeps: `an_animated_webp` 3123 MiB and `gif` 2184 MiB before the change to `budget_limits()`.
- Whole binary, one thread: 3187 MiB; default threads: 4223 MiB before the change.

## Verify commands and results
All cargo commands ran through `scripts/remote-run.sh imgtestmem` with the default cap.
- `cargo test -p ec-image --release` (default threads), run 1: exit 0
  (27/27 lib, 19/19 differential, 19/19 fuzz, 5/5 fuzz_regressions, 2/2 real_library).
- Same command, run 2: exit 0 (same counts).
- Same command, run 3: exit 0 (same counts).
- `cargo clippy -p ec-image --all-targets -- -D warnings`: exit 0.
- `rustfmt --check` reports differences in files this lane did not touch (already on main);
  not reformatted here.

## Left
- Fuzz-sweep robustness now runs under 1 GiB, not the 4 GiB default. The default path is covered
  only by `decode_animation_applies_the_default_limits`.
- The 3500M cap is still the lane server cap; the test binary peaks well under it now.
