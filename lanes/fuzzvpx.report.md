# Lane fuzzvpx: cargo-fuzz targets for ec-vp8 and ec-vp9

Goal: DONE.md gate 2, one cargo-fuzz target per untrusted VP8/VP9 entry point, smoke-run on
the servers. Base sha: `3e6d0111` (main).

## What changed

- `Cargo.toml` (root): `exclude = ["crates/*/fuzz"]` added to `[workspace]`.
- `crates/ec-vp8/fuzz/`
  - `Cargo.toml`: package `ec-vp8-fuzz`, `cargo-fuzz = true`, `libfuzzer-sys 0.4`, path dep on
    `ec-vp8`, `[workspace] members = ["."]`.
  - `fuzz_targets/decode_frame.rs`: one frame through `ec_vp8::decode::Decoder::decode`.
  - `fuzz_targets/decode_ivf.rs`: whole IVF through `ec_vp8::stream::decode_stream`.
  - `seed.sh`: rebuilds `corpus/` from `fixtures/vp8/*.ivf` (frames and whole/prefix IVFs).
  - `.gitignore`: `corpus/`, `artifacts/`.
- `crates/ec-vp9/fuzz/`
  - `Cargo.toml`: package `ec-vp9-fuzz`, same shape, path dep on `ec-vp9`.
  - `fuzz_targets/decode_frame.rs`: the whole input as one frame/superframe on a fresh
    `Decoder`, then the same bytes cut at 2 or 3 points (cut points chosen from the first byte)
    and decoded in order on one `Decoder` to exercise inter state.
  - `fuzz_targets/decode_stream.rs`: whole IVF through `ec_vp9::stream::decode_stream`.
  - `seed.sh`: rebuilds `corpus/` from `fixtures/vp9/*.ivf` and `crates/ec-vp9/tests/data/*.ivf`.
  - `.gitignore`: `corpus/`, `artifacts/`.

Seed corpus sizes (generated locally, not committed): vp8 95 frames + 14 IVFs; vp9 113 frames
+ 13 IVFs.

## Commands

Build (both crates, on a server):
`scripts/remote-run.sh fuzzvpx -- bash -c 'cd crates/ec-vp8 && cargo +nightly fuzz build'`
(and the same for `crates/ec-vp9`): both built clean.

Smoke runs (each target, 120 s):
`cargo +nightly fuzz run <t> -- -max_total_time=120 -rss_limit_mb=3000 -timeout=10`
run from `crates/<crate>/` inside `scripts/remote-run.sh fuzzvpx`.

## Smoke results

| target | runs | result |
|---|---|---|
| ec-vp8 `decode_frame` | stopped early | **CRASH** (see below) |
| ec-vp8 `decode_ivf` | 1195 in 124 s | pass, exit 0 |
| ec-vp9 `decode_frame` | 8722 in 121 s | pass, exit 0 |
| ec-vp9 `decode_stream` | 2399 in 121 s | pass, exit 0 |

### Crash: ec-vp8 `decode_frame`

- libFuzzer: `deadly signal` (exit 77) after a panic at `crates/ec-vp8/src/mc.rs:72:5`, which
  is the first `debug_assert!(vis_w >= 1 && vis_h >= 1)` in `mc::extend_plane`. The reference
  frame extension (`decode.rs:489-491`) reaches it with a zero-width or zero-height plane.
  The likely source is a key-frame header with a zero dimension that the decoder accepts
  (not verified; the decoder was not touched).
- Reproducer (base64, 32 bytes, `crash-4060390d42f351775b974bd9b72e3e9f647c000e` on the server
  under `crates/ec-vp8/fuzz/artifacts/decode_frame/`):
  `EAIAnQEqEBAAAAAHCIWFiIWEiAAoEABgawD+/zGrUIA=`
- Per DONE.md this crash must become a regression test once the decoder is fixed. Not done
  here: the decoder is out of scope for this lane. Fix belongs to an ec-vp8 decoder lane. The
  artifact is gitignored and stays on the host.

## Not run

- Full 1 CPU-hour per target (DONE.md item 2). Only the 120 s smoke was run.
- `cargo clippy` on the fuzz crates: they are outside the main workspace and not part of the
  `-D warnings` gate.

## Left

- Fix the ec-vp8 `extend_plane` zero-size path (decoder side) and add the regression test.
- Longer runs (1 CPU-hour each) once the vp8 crash is fixed.
