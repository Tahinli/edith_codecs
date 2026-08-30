VERDICT: FIXED — `a_real_aomenc_stream_with_an_altref2_reference_decodes_pixel_exact` now fires ALTREF2 reliably (3-4 hits per run observed across 6 consecutive runs, 0 misses) and hard-asserts on the hit count; full lib suite stays 224 passed / 0 failed.

## Root cause
The gate's flags (`--auto-alt-ref=1 --lag-in-frames=16 --enable-fwd-kf=0`) were
already sufficient for aomenc to build a hierarchical GF pyramid with a
level-2 ALTREF2 slot. The only wrong knob was `frame_count=16`: a 16-frame
clip gives aomenc only one mini-GOP of lookahead, room for exactly one
level (BWDREF), never a second (ALTREF2). No pyramid-height flag
(`--gf-min/max-pyr-height`) was actually required — just enough frames for
the existing lag to matter.

## Search table (direct aomenc probes, `~/.cache/aom-oracle/build/aomenc`,
seed 42, gate's own base flag set, `decode::ref_hits(ALTREF2_FRAME)` before/after
`decode_stream`)

| recipe | frames | result |
|---|---|---|
| baseline: `--auto-alt-ref=1 --lag-in-frames=16 --enable-fwd-kf=0` | 16 | altref2_hits 0->0 (matches the 86-attempt production history) |
| same flags, clip lengthened | 64 | altref2_hits 0->4 (FIRST POSITIVE) |
| `+--gf-min-pyr-height=2 --gf-max-pyr-height=4`, lag=25 | 64 | altref2_hits 4->6 (delta 2, no better than plain lag16/64f) |
| `+--gf-min-pyr-height=2 --gf-max-pyr-height=5`, lag=35 | 96 | altref2_hits 6->10 (delta 4) |
| `+--gf-max-pyr-height=5 --min-gf-interval=16 --max-gf-interval=32`, lag=35 | 96 | decode error: `allow_screen_content_tools` refusal (a real, separate, already-known capability gap — not an ALTREF2 problem) |
| `+--gf-max-pyr-height=5`, lag=48 | 128 | altref2_hits 10->10 (delta 0 — no improvement over the 96-frame case; diminishing once lag/frames are already ample) |

Conclusion: pyramid-height/gf-interval flags add nothing measurable over the
gate's own existing flags once the clip is long enough. **The fix is
`frame_count: 16 -> 64`**, nothing else in the flag set changed.

## Gate change
`crates/ec-av1/src/stream.rs`, `a_real_aomenc_stream_with_an_altref2_reference_decodes_pixel_exact`:
- `frame_count` 16 -> 64 (attempts stays 120, flags unchanged).
- Hard assertion added: `decode::ref_hits(ALTREF2_FRAME)` is snapshotted
  before calling the shared `a_real_aomenc_single_ref_gate` helper and the
  test now `assert!`s it advanced afterward, with a message naming the
  `gate-blind-to-feature` class. Previously the helper only `eprintln!`s a
  SKIP line and returns `ok()` when the feature never fires across every
  attempt — the exact prior 86-attempt-zero-firing history would have kept
  passing silently forever. The shared helper itself is left unchanged
  (other callers — LAST2/LAST3/BWDREF/ALTREF — still soft-skip; widening
  that is out of this lane's scope, not attempted here).

Verified firing count is not a fluke: 5 separate `cargo test ... --nocapture`
runs of just this test, seeds landing anywhere in `42..82` (`42 + attempt %
40`), all fired within the first handful of attempts:
```
FIRING seed 49: ALTREF2_FRAME hits advanced by 3
FIRING seed 45: ALTREF2_FRAME hits advanced by 3
FIRING seed 42: ALTREF2_FRAME hits advanced by 4
FIRING seed 42: ALTREF2_FRAME hits advanced by 1
FIRING seed 45: ALTREF2_FRAME hits advanced by 2
```
Every firing run also passed the existing pixel-exact-vs-ffmpeg check inside
`a_real_aomenc_single_ref_gate`, i.e. this decoder's ALTREF2 handling
(`decode.rs:4657`/`4676`, `mvstack.rs:1738`/`1775`/`1800`/`1853`) is not just
exercised but bit-exact on these streams. No real decode defect found — this
lane's finding is purely a dead gate, now live.

## Second target: `a_real_aomenc_filter_intra_stream_decodes_pixel_exact`
Status per charter step 6, report-only (lane-screen owns the fix). Ran 4x:
3/4 runs fired `filter_intra_hits() > 0` and matched ffmpeg pixel-exact;
1/4 SKIPped with the already-documented, separately-tracked refusal
`AV1 decode_stream (a frame with loop restoration enabled (this decoder
never reads the per-unit lr symbols))` — a *different* symptom than the
`allow_screen_content_tools` one this same gate's own doc-comment names, but
consistent with the doc-comment's framing that `screen_content_tools_
determination` is a real internal libaom trial-encode that varies run to
run even at fixed seed/cq (`cq-level=40`, `cpu-used=0`, no explicit
`--threads=1` pin unlike the other gates in this file — that is plausibly
the actual nondeterminism source, unconfirmed, out of scope here). Not
touched; this is exactly the class lane-screen is already narrowing in the
sibling `-screen` worktree.

## Verification
`env CARGO_TARGET_DIR=$HOME/.cache/cargo-target-altref2 CARGO_BUILD_JOBS=4 nice -n 19 cargo test -p ec-av1 --release --lib -- --nocapture` (EC_AV1_REQUIRE_AOMENC=1): 224 passed, 0 failed, 17 ignored, 73.13s.
