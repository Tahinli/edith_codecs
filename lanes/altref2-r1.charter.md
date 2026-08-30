# lane-altref2 r1 charter — make two vacuous gates fire

## Where you are
Worktree /home/tahinli/Documents/Code/Rust/edith_codecs-altref2, branch lane-altref2 @ main.
Build/test ONLY:
  `env CARGO_TARGET_DIR=$HOME/.cache/cargo-target-altref2 CARGO_BUILD_JOBS=4 nice -n 19 cargo test -p ec-av1 --release --lib <name> -- --nocapture`
FOREGROUND, EC_AV1_REQUIRE_AOMENC=1. Never push. Never touch the main checkout
or the sibling -gm / -screen / -recttx worktrees (three lanes are live there).
WIP COMMIT after every green milestone.

## The problem
`a_real_aomenc_stream_with_an_altref2_reference_decodes_pixel_exact` passes
without ever having exercised ALTREF2: 86 attempts, zero firings. A gate that
cannot fire the feature it names measures nothing, and this repo has been
burned by exactly that (class `gate-blind-to-feature`) more than once. The
recipe uses `--auto-alt-ref=1 --lag-in-frames=16`, which is enough for BWDREF
but evidently not for ALTREF2.

## What to do
1. Instrument first, guess second. Find how the gate proves a firing today
   (`crate::mvstack::ALTREF2_FRAME` is passed into a shared helper — read that
   helper and see what counter it checks). Then drive `aomenc` DIRECTLY from a
   shell, outside the test, and find a flag set that makes real ALTREF2
   references appear. The oracle's own `aomdec` at ~/.cache/aom-oracle/build is
   instrumented (`EC_TRACE=1`); `~/.cache/aom-oracle/build/aomenc --help` lists
   every knob. ALTREF2 comes from a multi-layer alt-ref pyramid, so the
   pyramid-height knobs (`--gf-min-pyr-height` / `--gf-max-pyr-height`), the
   GF group length knobs, the lag, and the frame count are the search space.
   A longer clip is likely required — 16 frames may simply be too few for
   aomenc to build a 2-level pyramid.
2. Report the search as a TABLE: flag set -> did ALTREF2 appear, so the next
   person does not repeat it. Negative rows are as valuable as the positive one.
3. Once a firing recipe exists, put it in the gate and make the firing
   HARD-ASSERTED: the test must FAIL if the count does not advance, not skip.
   Follow whatever the already-landed gates do (`wii_hits`, `partab_hits`,
   `rect_partition_hits` are precedents — grep for one and copy its shape).
4. If our decoder then refuses or mismatches on those streams, that is a REAL
   find, not a failure of this lane: pin it with
   `EC_AV1_GATE_DUMP=/tmp/claude-1000/altref2-flake-N.obu`, report the pin and
   the refusal string, and leave the gate asserting the firing with the decode
   part narrowed. Do NOT guess-fix a decode bug in this lane.
5. Bound every ffmpeg source you write with `-t <seconds>` on the OUTPUT side.
   A source's own `duration=` is not enough for every filter, and an unbounded
   lavfi source deadlocks the y4m pipe until the test times out — that has
   happened three times in this repo.
6. Second target, same treatment if budget allows: check whether
   `a_real_aomenc_filter_intra_stream_decodes_pixel_exact` still skips. It was
   eaten by the whole-frame `allow_screen_content_tools` refusal; lane-screen
   is narrowing that refusal in a sibling worktree, so just REPORT its status,
   do not fix it here.

## Done criteria
The ALTREF2 gate fires and hard-asserts, or the report proves with a search
table that aomenc will not produce ALTREF2 under any envelope this decoder can
otherwise decode (and then the gate is renamed/retired rather than left lying).
Full lib suite stays 224 passed / 0 failed. REPORT `lanes/altref2-r1.report.md`,
VERDICT on the FIRST line, with the search table and the firing count.
