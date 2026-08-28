# lane-warp r5 charter — pin refuses on an "impossible" partition after the comppin swap fix

## Where you are
Worktree /home/tahinli/Documents/Code/Rust/edith_codecs-warp, branch lane-warp @ c3c8ec8
(rebased onto main e1b1543, which HAS the comppin fix: enable_interintra_compound/enable_jnt_comp
un-swapped at the decode_stream call site — do NOT re-investigate that; it is fixed and merged).
Build/test ONLY with: `env CARGO_TARGET_DIR=$HOME/.cache/cargo-target-warp cargo test -p ec-av1 --release --lib <name> -- --ignored --nocapture`
Never plain `cargo test` on the whole workspace. Never touch another worktree.

## The r5 baseline fact (reproduce FIRST, 1 run)
`pinned_warp_stream_decodes_pixel_exact` (crates/ec-av1/src/stream.rs:3467, fixture
fixtures/warp-mismatch.obu, 621 bytes, aomenc seed-49 warped-motion stream, 64x64, 24 frames):
FAILS with `Unsupported { what: "AV1 tile", why: "a partition type this encoder never writes" }`
from the INTER tile walk refusal at crates/ec-av1/src/decode.rs:7581 (the `_ =>` arm).
With EC_AV1_TRACE=1: frame 0 (key) walks fine (partition_w64 value=3 then w32 zeros);
in the inter frame the last good block is `EC_TRACE mi_row=8 mi_col=0 skip=1 is_inter=1 mv=(6,8)
is_new_mv=true bsize=32 ref=4 filter=[0,0] motion_mode_eligible=1 obmc_selected=0 tell=200`,
then the next partition symbol read hits the refusal.
Historical: BEFORE the swap fix this same stream decoded all 24 frames and mismatched frame-1
luma pixels — i.e. under the old (wrong) alignment the partition symbols happened to decode to
supported values. So either (a) lane-warp's own symbol consumption between assign_mv and the next
partition read is still wrong (r4 landed WARPED_CAUSAL 3-symbol read_motion_mode + num_proj_ref;
sites: decode.rs:4287 num_proj_ref, decode.rs:4826/5638 read_motion_mode), or (b) alignment is now
CORRECT and the stream genuinely contains an extended partition type (HORZ_A/B, VERT_A/B,
HORZ_4/VERT_4) our inter walk refuses — a capability gap, not a desync.

## Decide (a) vs (b) FIRST — do not start porting partitions blind
Ground truth: `ffmpeg -i fixtures/warp-mismatch.obu` decodes it (the pin test itself uses
ffmpeg as oracle), so the stream is valid. Decisive checks, cheapest first:
1. Read the partition VALUE at the refusal: add the ctx+value to the refusal-site trace
   (mirror the TRACE partition_w8 style at decode.rs:3603) at BOTH refusal arms (decode.rs:3646
   region and 7581). A value in 4..=9 with a PLAUSIBLE ctx = likely (b); a garbage-looking
   sequence (e.g. partition after only 2 of 4 32x32 slots walked) = (a).
2. Count symbols per block vs libaom read order (class symbol-consumption-gap: count reads per
   block BEFORE tracing one symbol). The block before the refusal is motion_mode_eligible=1,
   bsize=32, single-ref (ref=4): confirm we read motion_mode as the 3-symbol WARPED alphabet
   exactly when libaom does (libaom read_motion_mode: SIMPLE unless num_proj_ref>=1 &&
   allow_warped_motion && !large-mv...); check num_proj_ref's gating conditions against
   libaom av1_findSamples/av1_selectSamples semantics — an eligibility disagreement on THIS
   block shifts every later symbol.
3. If still ambiguous: cross-decoder RANGE compare (class compare-range-not-tell — msac RANGE
   after each element, NEVER tell() absolute values; dav1d/aomdec with a state dump, or the
   existing EC_AV1_TELL checkpoint machinery, env-gated, already in decode.rs).

## Then fix
- If (a): fix the consumption gap; pin must go 24/24 pixel-exact (the test asserts vs ffmpeg).
- If (b): DO NOT silently implement all extended partitions in one round. Report the exact
  partition type(s) the stream uses; implement ONLY those in the inter walk (decode.rs inter
  partition match ~7000-7581), reusing the existing NONE/SPLIT/HORZ/VERT leaf plumbing;
  every new arm threads neighbours/partition-ctx exactly as SPLIT does (class
  tx-class-cross-cutting: grep every surface indexed by partition kind — scan, ctx, records —
  and sweep in ONE round).

## Done criteria (ALL, evidence in the report)
1. `pinned_warp_stream_decodes_pixel_exact` PASSES (24/24 frames, pixel-exact vs ffmpeg),
   run twice, both green, output excerpt in report.
2. `cargo test -p ec-av1 --release --lib` (whole crate lib suite) green; paste the final
   `test result:` line.
3. WIP committed to lane-warp (compiling; message prefix `wip(av1): warp r5 ...` stating what
   was found and what changed). Commit even on a HANDOFF.
4. REPORT FILE written to lanes/warp-r5.report.md in THIS worktree: verdict line first
   (`PASS` or `HANDOFF: <state>`), then (a)-vs-(b) determination WITH the trace evidence,
   then what changed, then anything owed. An empty or vague report = the round is void.

## Hard rules
- ≤60 tool calls; if running out: commit + write the report as HANDOFF with exact resume state.
- Never `git checkout` a file with uncommitted work in it; never touch fixtures/ contents,
  main repo, or other worktrees; never push.
- The refusal string is a CLAIM (class refusal-strings-are-claims): it may only survive if a
  test proves the capability genuinely absent from the stream — here ffmpeg already decodes
  the stream, so the refusal cannot stand as-is.
