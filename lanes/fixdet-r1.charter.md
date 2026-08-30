# lane-fixdet r1 charter — the gate fixtures are not reproducible

## Where you are
Worktree /home/tahinli/Documents/Code/Rust/edith_codecs-fixdet, branch
lane-fixdet @ main.
Build/test ONLY:
  `env CARGO_TARGET_DIR=$HOME/.cache/cargo-target-fixdet CARGO_BUILD_JOBS=4 nice -n 19 cargo test -p ec-av1 --release --lib <name> -- --nocapture`
FOREGROUND, EC_AV1_REQUIRE_AOMENC=1 on gates. Never push. Never touch the main
checkout or the sibling -rectwire2 worktree. WIP COMMIT after every green step.

## The measurement (already done — do not repeat it, build on it)
ffmpeg's `gradients` lavfi source IGNORES its `seed` parameter for colour
selection. Hashing its output five times with an identical command line:

    e981f6da916b400a  2451ed66cdf1c114  13c53f4382ab72a1
    f9ac6e84c77489ff  dcffa3d8caa39e78

Five runs, five different fixtures. Twenty gate sites in
`crates/ec-av1/src/stream.rs` build their fixture this way, so those gates have
never had a reproducible input.

Two consequences, both measured:
- `a_real_aomenc_filter_intra_stream_decodes_pixel_exact` fails about 1 run in
  15 even in isolation with the encoder pinned to one thread. The encoder is
  NOT the source of that: hashing aomenc's output six times over a FIXED y4m
  gives the same bytes every time. The fixture is the variable.
- Every "attempt-selection flake" attributed to aomenc's RD search deserves
  re-examination. Some of those were the SOURCE changing under the gate.

Naming a seed that does nothing is worse than having no seed: it advertises
reproducibility the gate does not have, and a pin captured from such a run
cannot be regenerated.

## The fix
`gradients` IS deterministic once the colours are given explicitly — verified,
three identical hashes. But the gates need per-attempt VARIETY too: they sweep
`seed` across attempts precisely to sample different content, and freezing the
colours would make every attempt identical and quietly destroy the sampling
that makes features fire (this repo has a `sampler-decorrelated-gate` class
already).

So derive the colours FROM the seed. Verified working: seeds 42, 43, 42 gave
hashes c25eb1d4d042, 5097b017faf7, c25eb1d4d042 — different seeds differ, the
same seed reproduces exactly.

1. Write ONE helper in `stream.rs`, e.g.
   `fn gradients_source(seed: u32, w: usize, h: usize, duration: f64) -> String`,
   that maps the seed deterministically to `c0`..`c3` (and keeps passing
   `seed` so any future ffmpeg that honours it still varies). Document WHY in
   a comment: the seed alone does not make this source reproducible.
2. Route all 20 sites through it. Keep each site's existing width/height/
   duration/rate; this is a determinism fix, not a content change.
3. Prove it: a test that builds the same source string twice for one seed and
   runs ffmpeg on both, asserting byte-identical output, and that two
   different seeds differ. That test is the guard against this regressing.

## Then re-measure what the fix changes
- Run `a_real_aomenc_filter_intra_stream_decodes_pixel_exact` 20 times. It was
  14/15 before. Report the new number honestly, whatever it is.
- Run the free-partition gate 3 times and report whether its refusal/match
  counts are now stable run-to-run. If they are, say so; if they still move,
  aomenc attempt selection is real on top of this and the report should say
  that too, with the numbers.
- If a gate now fails CONSISTENTLY, that is a genuine decoder defect this
  nondeterminism was hiding. Do NOT paper over it: pin it with
  EC_AV1_GATE_DUMP, name it in the report, and refuse-by-name rather than
  guess-fix.

## Done criteria
Full lib suite 231 passed / 0 failed. All 20 sites through the helper. The
determinism test in place. REPORT `lanes/fixdet-r1.report.md`, VERDICT on the
FIRST line, with the before/after flake rates as measured counts.
