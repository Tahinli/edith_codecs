# lane-chroma r1 charter — the chroma intra modes this decoder still refuses

## Where you are
Worktree /home/tahinli/Documents/Code/Rust/edith_codecs-chroma, branch
lane-chroma @ main.
Build/test ONLY:
  `env CARGO_TARGET_DIR=$HOME/.cache/cargo-target-chroma CARGO_BUILD_JOBS=4 nice -n 19 cargo test -p ec-av1 --release --lib <name> -- --nocapture`
FOREGROUND, EC_AV1_REQUIRE_AOMENC=1 on gates. Never push. Never touch the main
checkout or the sibling -realworld worktree. WIP COMMIT after every green step.

## The measurement
Walking a DEFAULT-settings stream through its blockers, this is number 5 in the
chain — reached as soon as CDEF, delta-q and palette are out of the way:

    unsupported: AV1 tile (a smooth or paeth chroma mode (round 2))

at `decode.rs`:

    if (9..=12).contains(&uv_mode) {
        return Err(unsupported("a smooth or paeth chroma mode (round 2)"));
    }

There is a sibling refusal, "a directional chroma mode (round 2)", to check
alongside it.

## Why this may be much smaller than it looks — verify before assuming
`crates/ec-av1/src/intra.rs` ALREADY implements SMOOTH_PRED, SMOOTH_V, SMOOTH_H
and PAETH_PRED, and they are exercised for LUMA today. The chroma path may
simply be refusing modes whose predictor is already present and correct.

Do NOT assume that. Establish it:
1. Read the luma call path and the chroma call path side by side and list every
   difference — edge construction, availability, the `Reach`, subsampling, the
   CfL interaction (`UV_CFL_PRED` is mode 13 and is handled separately today).
2. Check against libaom what a chroma block of these modes actually needs that
   a luma block does not. `av1_predict_intra_block` is shared in libaom, which
   is evidence the predictor is shared, but the EDGE setup differs for a
   subsampled plane.
3. If it really is just the refusal, removing it is a two-line change and the
   whole value of the round is the PROOF — a gate with a hard-asserted firing
   count showing real aomenc streams decode those modes pixel-exact.

The "(round 2)" suffix means an earlier round deliberately deferred these. Find
out whether it deferred them because they were hard or merely because they were
out of that round's scope — `git log -S "smooth or paeth chroma"` will say.

## Method
Refuse-by-name first, decode second. On a pixel mismatch, self-pin with
`EC_AV1_GATE_DUMP` and compare the msac RANGE (never `tell()`) against the
instrumented oracle: `EC_TRACE=1`, `EC_TRACE_MODE=1` (covers intra key-frame
mode info too), `EC_TRACE_COEFF=1`. If the oracle's range is UNCHANGED where
ours moves, we read a symbol it never wrote — check that before suspecting a
predictor.

Note these modes affect PIXELS, not symbol flow, if the mode symbol is already
being read correctly today — so a mismatch here is most likely a prediction
defect, and the recon dump (`EC_AV1_PREFILT_DUMP` on both sides) is the faster
instrument than the range ladder.

## Done criteria
Full lib suite stays green (232 passed / 0 failed on main today). Smooth and
Paeth chroma decode pixel-exact under a gate that HARD-ASSERTS its firing count,
or the report explains precisely what a chroma block needs that is missing.
Report on the directional-chroma sibling too, even if you do not land it.
REPORT `lanes/chroma-r1.report.md`, VERDICT on the FIRST line.
