# Lane vp8fuzzfix1: ec-vp8 decode_frame crash

Base sha: b6e77f30 (main).

## Goal
Fix the `decode_frame` fuzz crash in ec-vp8: `debug_assert!(vis_w >= 1 && vis_h >= 1)` in
`mc::extend_plane` reached with a zero-size plane.

## Cause
The reproducer is a key frame whose header declares height 0 (width 0x1010, height 0).
`parse_keyframe_dims` accepted it, so `make_ref` built a zero-row reference plane and
`extend_plane` got `vis_h == 0`. A second path, an inter frame before any key frame, was already
rejected, but only after header parsing had mutated persisted state.

## Changes
- `src/frame.rs` `parse_keyframe_dims`: zero width or height returns `Error::corrupt`.
- `src/decode.rs` `decode()`: inter frame with no decoded key frame returns `Error::corrupt`
  before `FrameHeader::parse` touches state. New helper `has_reference_frames()`, also used by
  `decode_interframe`.
- `tests/fuzz_regressions.rs` (new): reproducer decodes to Err with no panic; zero-width key frame
  rejected; inter frame before key frame rejected.

## Verify
- `scripts/remote-run.sh vp8fuzzfix1 -- cargo clippy -p ec-vp8 --all-targets -- -D warnings`: pass (exit 0).
- `scripts/remote-run.sh vp8fuzzfix1 -- cargo test -p ec-vp8 --test fuzz_regressions`: 3/3 pass.
- `scripts/remote-run.sh vp8fuzzfix1 -- cargo test -p ec-vp8 --release`: all test binaries pass
  (57 unit, 3, 4, 4, 2, 3; 0 failed).
- Fuzz: `decode_frame`, `-max_total_time=600 -rss_limit_mb=3000 -timeout=10`, seeded corpus:
  5217 runs in 601 s, exit 0, no crash artifacts.

## Left
- Crashes found: 1 (fixed). No further crashes in the 600 s run.
- Not checked: whether the regression tests fail on the pre-fix base (only verified passing on the fix).
