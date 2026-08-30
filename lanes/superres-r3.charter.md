# lane-superres r3 — close the 5-pixel residue by pinning the REAL scale case

Worktree `/home/tahinli/Documents/Code/Rust/edith_codecs-superres`, branch
`lane-superres`, at 2022b4c. Read `lanes/superres.report.md` first — it is
short, accurate, and lists what r2 already ruled out. Do not re-derive it
(class `worker-cap-spent-reading`).

## State
Landed and committed: refusal-by-name (79252c8), the spec 7.16 upscaler pinned
against real libaom (aec76d1), stage-3 wiring (150bbdc), and r2's two real
fixes (85c070b): the gate needed `--superres-kf-denominator=12` (libaom's
`--superres-denominator` only controls NON-key frames, and this fixture makes
every frame a key frame, so `use_superres` was silently decoding false), and
`decode.rs` cropped chroma with floor `fw / 2` where AV1 wants
`ROUND_POWER_OF_TWO(fw, 1)` — a real general decoder bug, swept across both
constructors of the cropped `Picture`.

Still failing: `a_real_aomenc_superres_key_frame_sequence_decodes_pixel_exact`,
5 of 4096 luma pixels off by exactly 1, all at x in 59..62 of a 64-wide frame.

## The decisive move, before any theory
r1 pinned the upscaler against libaom for **in8->out12 and in8->out16**. The
failing case is **43 -> 64**. A kernel pinned at two small symmetric ratios says
nothing about the phase/step arithmetic at the ratio that actually fails —
class `instrument-at-bound`: a search result at its own edge is not a result.

So: extend `scripts/superres-pin-harness.c` (committed, builds against
`~/.cache/aom-oracle/build/libaom.a`) to run the EXACT failing case — input
width 43, output width 64, 8-bit, with the same row bytes our decoder holds at
the failing row — through the real exported
`av1_get_upscale_convolve_step` / `av1_convolve_horiz_rs_c` /
`av1_resize_filter_normative`, and print every output column. Compare column by
column with ours. That either localises the defect to a specific
`x_qn` / `int_pel` / `filter_idx` at x=59..62, or proves our kernel is right at
this ratio and moves the suspicion to what we feed it.

Two specific things to check in that harness run, because they are where the
ratio matters:
- `x0_qn` initialisation (libaom `get_upscale_convolve_x0`) — the starting
  phase, which the symmetric ratios r1 pinned may make degenerate.
- The right-edge source clamp. r2's theory is that we edge-replicate from the
  cropped row where libaom uses a real decoded pixel. Note that libaom runs
  `av1_extend_frame_borders` before `av1_superres_upscale`, which replicates
  from column `y_crop_width - 1` — i.e. replication is probably RIGHT and the
  theory probably wrong. Let the harness settle it rather than the reasoning:
  feed it a row whose column 43+ differs from a replicate of column 42 and see
  whether the reference output changes at all.

## Then
1. Fix whatever the harness localises; the gate must decode pixel-exact and
   hard-assert `superres_hits() > 0`. COMMIT.
2. Full scoped suite `EC_AV1_REQUIRE_AOMENC=1 nice -n 19 cargo test -p ec-av1 --lib -j4`,
   timeout >= 600000 ms. Baseline on this tree 232/0; main is at 234 with two
   new guard tests you inherit at merge. r2 never ran the suite after landing
   the `decode.rs` chroma-crop fix, so this run is also that fix's first real
   check. COMMIT.
3. Stage 4 — inter-frame superres, scaled-reference MC (spec 7.11.3.3, libaom
   `av1_setup_scale_factors_for_frame` / `av1_convolve_2d_scale`). Its own gate,
   its own firing counter, its own commit.
4. Update `lanes/superres.report.md`, VERDICT on the first line.

## Hard rules
- `export CARGO_TARGET_DIR=$HOME/.cache/cargo-target-superres`. Foreground
  builds, `nice -n 19 cargo ... -j4`; every `cargo test` gets a timeout of at
  least 600000 ms.
- Sibling worktrees (edith_codecs, -chroma, -realworld, -lr, -tiles, -palette)
  have live agents. Never build in or edit them.
- NEVER push, never merge, never touch `main`. Commit on `lane-superres` only.
- 75-turn cap and it does NOT reset on resume: commit at every green step, and
  commit the extended harness itself — it belongs in `scripts/`, not in a
  scratchpad that gets reaped (class `oracle-in-reaped-dir`).
