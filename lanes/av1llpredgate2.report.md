# lane-av1llpredgate2: pin the debug-exact 6-frame stream for the ss-aware `obmc_skip_chroma_above` fix

Tree: lane-av1-llpredgate2 @ 041e3eae (the lane-av1-llpred2 fix, under
review — no code change landed here). This lane adds ONLY the regression
gate.

## Regression gate

`stream::tests::a_lossless_444_min_partition8_inter_stream_decodes_sample_exact`
(crates/ec-av1/src/stream.rs), modelled on
`a_lossless_444_min_partition64_inter_stream_decodes_pixel_exact`.

### Fixture provenance

- Origin: lanes/av1llpred.report.md / lanes/av1llpred2.report.md stream —
  `testsrc2=size=128x96:rate=?` ffmpeg source, yuv444p, encoded by the
  shared aomenc (`$HOME/.cache/aom-oracle/build`, pre-2026 build) with
  `--profile=1 --lossless=1 --enable-palette=0 --enable-intrabc=0
  --min-partition-size=8 --max-partition-size=64`, 6 frames.
- Committed at `crates/ec-av1/fixtures/ll444_minp8_inter.obu`
  (gitignored pattern `fixtures`, force-added like every sibling fixture;
  on-disk == `git ls-tree` blob verified at commit time).
- 34037 bytes, sha256
  `04fb6d38de5382e647bfb70b6a17802f1fccc5502e6698c0394167891f2b27dc`
  (= the /tmp/llintra8 original, byte-identical copy),
  FNV-1a64 `0x2c8f4a2cc02552da`. Both pinned as test consts; missing
  file panics — no skip path.

### Debug build path, not release-only

The defect this gate pins is a DEBUG-only panic: the pre-llpred2 ratio
(`(write_h / chroma_side).trailing_zeros()` = 64 at 4:4:4) shifted by 64,
which panics under debug_assertions and silently wraps in release
(release was already byte-exact). The test runs under `cargo test`'s
default dev profile — the profile where the parent measured byte-exact
and where the old code dies — so a red run names the class instead of
silently passing.

### Counter disposition: cited unit check, not a skip-arm counter

A skip-arm counter was added and measured, then removed:

- With the counter bumped at the ONE site the ss-aware decision returns
  true (after the `obmc_skip_chroma_above` call in `obmc_plan`), the
  pinned stream measured `OBMC_SS_SKIP_CHROMA_ABOVE_HITS 0 -> 0` while
  OBMC itself fired (`OBMC_HITS 0 -> 77`, `OBMC_RECT_LEAF_HITS 0 -> 20`).
  At 4:4:4 the skip needs a 4x4/8x4/4x8 PLANE block; min-partition-8
  never codes one, so the skip arm cannot fire on this stream and any
  `delta > 0` assert would be permanently red — vacuity in reverse.
- Per the ticket's fallback: the decision TABLE is pinned by the existing
  unit check
  `decode::obmc_skip_chroma_tests::rect_strips_skip_above_chroma_at_420_and_shift_never_overflows`
  (4:2:0 16x8/8x16 strips still skip; 4:4:4 shifts never leave `usize`),
  and the stream gate pins the END-TO-END behaviour sample-for-sample.

### Assertions (all before/without any skip path)

1. Fixture bytes: exact length + FNV-1a64.
2. `decode_stream` of the pinned bytes succeeds (a returning `>> 64`
   panic fails here) — 6 frames, 128x96.
3. Oracle aomdec (`--codec=av1 --rawvideo`): raw size 221184, then
   per-frame per-plane `assert_eq!(bad, 0)` — 0 differing samples,
   message carries class `obmc-ss-blind-chroma-above`.
4. ffmpeg arm (`ffmpeg_decode_sequence_444`): same sample-exact compare.
   Neither arm skipped in the green run (aomdec + ffmpeg both present).

## Red probes (mutation verification)

1. Old-ratio probe: callsite swapped to the exact 71f07edd derivation
   (`(write_w|write_h per chroma_side).trailing_zeros()` pair) → test
   RED: `attempt to shift right with overflow` at decode.rs
   `obmc_skip_chroma_above` — the debug-panic class reproduced.
2. Chroma-diff probe: `skip_chroma_above = true` forced at the callsite
   (re-blend class) → test RED at the named assert: `aomdec frame 1
   plane u: 46 samples differ (class obmc-ss-blind-chroma-above)`.
3. decode.rs restored byte-identical after both probes
   (`git diff --stat`: only stream.rs + the fixture).

## Non-regression / hygiene

- `cargo test -p ec-av1 --lib
  a_lossless_444_min_partition8_inter_stream_decodes_sample_exact`
  (CARGO_TARGET_DIR=$HOME/.cache/cargo-target-av1llpredgate2, dev
  profile): 1 passed, 0 failed, no SKIP lines.
- `cargo check -p ec-av1 --all-targets`: 0 warnings, 0 errors.
- The skip expression itself is untouched (no diff in decode.rs); the
  reserved 4:2:0 group-tail chroma arm untouched; no VPS, no full suite.
