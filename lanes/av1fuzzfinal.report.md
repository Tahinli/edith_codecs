# lane av1fuzzfinal: ec-av1 fuzz gates

Base: main f5eb855b. Branch lane/av1fuzzfinal. Commits: f7128696 (items 17/18), 10deb9bb (MV range crash).

## Items 17 and 18 (fuzzh265av1 report)
- Reproduced on the single-thread harness path (`decode_stream_with_threads(data, 1, ...)`, the path the
  fuzz target uses). The earlier replay tests used the default thread count and asserted nothing about the
  result, so they never exercised the panic.
- Item 18 (`decode_stream-palette-pred-size.bin`): a 4x4 palette/intrabc override reached an 8x8 transform
  block; `reconstruct` indexed it past its end. Root cause: the override is a single per-frame slot set at
  parse time and taken at the next intra push. A size mismatch means the slot belongs to another block. A
  conforming stream cannot produce this.
- Item 17 (`decode_stream-cfl-ac-size.bin`): the input does not reach a CfL-size mismatch first. Its first
  inconsistent block is a 8x8 palette override on a 16x16 transform block (the same class as item 18). The
  CfL AC-size check is in place but was not separately exercised by this input. The earlier "CfL" diagnosis
  was wrong.
- Fix: `exec_intra` checks the override and the CfL AC size against the transform area and calls
  `FrameCtx::flag_corrupt`. `decode_frame` returns `Error::Corrupt` when the flag is set. The
  `unwrap_or(0)` / ignore-override fallbacks stay as a no-panic backstop.
- Replay tests (tests/fuzz_regressions.rs): `cfl_transform_larger_than_ac_buffer_is_corrupt`,
  `palette_override_of_another_size_is_corrupt`. Both assert `Error::Corrupt`.
- Inventory: FrameCtx grew 1296 -> 1312 B and Av1Encoder 2944 -> 2960 B (stack_budget.rs rows updated).

## New crash found by the 1 h decode_stream run (run1)
- `mvstack.rs:179` debug assertion in `mv16`: "mv outside the spec's i16 range: (-16544, -35328)".
  Input `decode_stream-mv-i16-range.bin` (109 bytes). Path: intrabc DV read, `read_mv`, then
  `record_intrabc_mi_rect`, then `mv16`. Release builds wrapped the value silently.
- Fix: `read_mv` checks each component against (-(1<<14), 1<<14), flags the frame, and clamps. `fctx` is
  threaded into `read_mv` and `assign_compound_mv`.
- Replay test: `intrabc_vector_outside_the_spec_range_is_corrupt`.

## Verify (final code, server)
- `EC_MEMGUARD_MAX=6G cargo test --release -p ec-av1 -- --test-threads=4` on 10deb9bb:
  **811 passed, 0 failed, 49 ignored** (host c, 3528 s).
- Earlier run before the stack_budget table update: 810 passed, 1 failed
  (`stack_budget::the_measured_inventory_is_accurate`). Fixed by the table update; that module then passed
  8/8 and the full run above is clean.
- `cargo clippy -p ec-av1 --all-targets -- -D warnings`: clean (host b, final tree).
- Fuzz `decode_stream` (`cargo +nightly fuzz run decode_stream -- -max_total_time=3600 -rss_limit_mb=2048`):
  - run1 on f7128696: crash at mv16 (above), exit 77.
  - run2 on 10deb9bb, host b: **960,909 runs in 3601 s, no crash, exit 0**. Clean.
- Fuzz `parse_obu` (1 h, host a): **21,470,442 runs in 3601 s, exit 0**. Run on f7128696. The
  `ec-av1-syntax` crate it links is unchanged by 10deb9bb, so this is the final code for that target.

## DONE.md
- decode_stream: 1 CPU-hour, no panic on the final tree: MET.
- parse_obu: 1 CPU-hour, no panic: MET (see note above on tree).
- Corrupt input returns Error::Corrupt for the four recorded crash inputs: MET (4 of 4 replay tests).

## Left
- The 4:2:2 CfL source arm (the open question in the fuzzh265av1 report) is not settled. The
  mismatch check only shows that the sizes disagree. It does not prove that a conforming stream can never
  reach it.
- The `decode_threads()` default path returned a different error for the cfl input than the one-thread
  path (forbidden-bit error in one run). This was seen only in an early probe and not investigated.
- Crash-input fixtures are committed under tests/data/fuzz.
