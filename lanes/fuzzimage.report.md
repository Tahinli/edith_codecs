# fuzzimage: cargo-fuzz targets for ec-image

Goal: DONE.md shared gate 2, at least one cargo-fuzz target per untrusted entry point in
ec-image (whole-file decode, limit-bounded decode, header info, animation, and each format module).
Base sha: 6718bc03 (main at worktree creation).

## What changed
- `crates/ec-image/fuzz/`: standalone cargo-fuzz crate (`ec-image-fuzz`, own `[workspace]`, path dep
  on ec-image). Same layout as `crates/ec-ogg/fuzz/`. Ten targets:
  - `decode`: `ec_image::decode` (format guessed from the signature), default limits.
  - `decode_with_limits`: same, under `Limits { max_pixels: 4096*4096, max_alloc: 64 MiB,
    max_total_alloc: 128 MiB }`.
  - `info`: `ec_image::info`.
  - `decode_animation`: `ec_image::decode_animation` (GIF and animated WebP frames).
  - `bmp`, `jpeg`, `png`, `tiff`: each module's `info` and `decode` under the small limits.
  - `gif`: `info`, `decode`, `decode_animation`. `webp`: `info`, `decode`, `decode_frames`.
  - `seed.sh`: every file in `fixtures/stills` (94 files) seeds the four whole-file targets; each
    format target gets only its own format's files (bmp 17, gif 6, jpeg 11, png 31, tiff 17, webp 12).
  - `.gitignore`: target, corpus, artifacts, coverage.
- `crates/ec-image/src/webp/vp8.rs`: the inverse DCT runs in i64. Found by fuzzing (see below).
- `crates/ec-image/src/png.rs`: the 16-bit expansion buffer is budgeted with `Limits::check_bytes`
  before it is allocated (it was unchecked; it can be twice the raw scanlines the existing check saw).
- `crates/ec-image/src/bmp.rs`: the output buffer (width x height x channels) is budgeted with
  `Limits::check_bytes` (it was unchecked). The RLE index buffer is `width * height`, covered by
  the same bound.
- `crates/ec-image/src/jpeg.rs`:
  - a scan that names no components is `Error::corrupt` (it indexed an empty vector). Found by fuzzing.
  - coefficient storage is budgeted across all components with one `AllocBudget`, not one buffer at
    a time; three 4:4:4 planes could each pass the per-buffer cap and add up to far past it.
  - the RGB output buffer is budgeted with `check_bytes`; `reconstruct` now takes `Limits`.
  - a single-component frame with no plane returns `Error::corrupt` instead of `expect`.
- `crates/ec-image/tests/fuzz_regressions.rs`: five regression tests. Two inline the minimised
  fuzzer inputs (a 650-byte WebP that drives the VP8 IDCT overflow, and a 90-byte BMP whose embedded
  JPEG has an empty scan). Three pin the new budget refusals (PNG expansion, BMP output, JPEG
  coefficients across components).
- Lint cleanup needed for the gate. The workspace toolchain (rustc/clippy 1.98) flagged 22
  pre-existing lints in `ec-image` (`chunks_exact` with a constant size, a needless lifetime, manual
  range patterns, a collapsible `if`, a manual checked division). They are fixed in place, with no
  behaviour change: `chunks_exact(N)` became `as_chunks::<N>().0.iter()`, and the rest are the
  mechanical rewrites clippy suggests. Two sites in `tests/differential.rs` got the same change.

## Bugs found and fixed
Fuzz pass 1 (120 s each, `-max_total_time=120 -rss_limit_mb=2048 -timeout=10`):

| target | exit | finding |
|---|---|---|
| decode | 77 (crash) | VP8 IDCT i32 overflow in `webp/vp8.rs` (panic in debug / fuzz builds) |
| decode_with_limits | 77 | same |
| decode_animation | 77 | same |
| webp | 77 | same |
| bmp | 77 | `jpeg.rs` index out of bounds: scan with zero components (embedded JPEG) |
| info, gif, jpeg, png, tiff | 0 | no crash |

- VP8 IDCT: the second pass multiplies intermediate values that a hostile block can drive past
  i32. The fix widens the arithmetic to i64 and truncates the result to i32 at the end. Valid
  streams never leave i32, so their output is unchanged. In a release build the old code wrapped
  silently rather than panicking, so the regression test only catches the bug in a debug build.
- Minimised inputs (the `cargo fuzz tmin` runs were done on a detached worktree of main, so the crash
  reproduced on the old code): the 650-byte WebP for the VP8 bug and the 90-byte BMP for the JPEG bug.
- The unchecked-allocation fixes (PNG, BMP, JPEG output, JPEG coefficient budget) were found by reading
  the decoders, not by fuzzing. Each has a test that fails on main and passes after the fix.

## Verify commands and results
All cargo commands ran through `scripts/remote-run.sh fuzzimage` on host a or b.

- `cargo +nightly fuzz build --fuzz-dir crates/ec-image/fuzz`: exit 0, all ten targets built.
- Pass 1 (above): five crashes, all fixed.
- Pass 2, after the fixes, 120 s each, same flags:

| target | exit | executions | crash/timeout/OOM |
|---|---|---:|---|
| decode | 0 | 13,798 | none |
| decode_with_limits | 0 | 17,124 | none |
| info | 0 | 3,683,798 | none |
| decode_animation | 0 | 15,300 | none |
| bmp | 0 | 70,771 | none |
| gif | 0 | 28,748 | none |
| jpeg | 0 | 8,365 | none |
| png | 0 | 394,575 | none |
| tiff | 0 | 290,053 | none |
| webp | 0 | 9,862 | none |

- Regression tests on main (pre-fix, `fuzz_regressions.rs` copied into a detached worktree):
  `cargo test -p ec-image --test fuzz_regressions`: exit 101, 5 of 5 failed, as intended (the
  JPEG and VP8 tests panicked on overflow or indexing; the three budget tests failed their assertions).
- `cargo clippy -p ec-image --all-targets -- -D warnings`: exit 0 (clean).
- `cargo test -p ec-image` (debug, default threads, `EC_MEMGUARD_MAX=6G`): exit 0. Unit 27 passed,
  differential 19 passed, fuzz 18 passed (about 500 s), fuzz_regressions 5 passed, real_library 2
  passed, doctests 0.
- `cargo test -p ec-image --release -- --test-threads=1` (`EC_MEMGUARD_MAX=6G`): exit 0. Unit 27,
  differential 19, fuzz 18 (79 s), fuzz_regressions 5, real_library 2.
- `cargo test -p ec-image --release` with default threads: exit 101. The `fuzz` binary is SIGKILLed
  by the memory guard (`a_gif_with_many_tiny_frames_over_a_huge_canvas_is_refused_by_the_total_budget`
  holds several GiB of canvases while the other tests run in parallel). This is pre-existing: the same
  GIT test was killed on main under the default 3500M guard too. Under the default threads, a 6G cap
  does not help. The single-threaded run passes.

## Left open
- The gate asks for 1 CPU-hour per target. Only 120 s smoke runs were done. The gate is not met.
- The corpus is seeded from fixtures only. No minimised corpus and no coverage measured.
- `fuzz_regressions` pins the budgets with hand-built containers. The fuzzer does not reach the
  budget refusals, because the seeds are small real files.
- `cargo test -p ec-image --release` with default threads needs more memory than the lane's 3500M
  guard. A fix belongs in `tests/fuzz.rs` (lower the GIT budget test's canvas or serialise it), which
  is outside this lane's scope, so it is not changed here.
- `crates/ec-image/fuzz/Cargo.lock` was generated on the server and is not in this commit.
