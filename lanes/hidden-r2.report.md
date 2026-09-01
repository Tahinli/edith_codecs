# lane-hidden r2 — hidden-frame compare wired into the 15 alt-ref gates

## What changed
- `crates/ec-av1/src/stream.rs` — 13 insertion sites, each **after** the gate's existing
  shown-frame `assert_eq!` loop, calling r1's `decode_all_frames_vs_oracle(&stream, NAME)`
  on the SAME stream bytes: every decode-order frame (hidden alt-refs included) byte-compared
  against the instrumented `aomdec`'s `EC_AV1_FINAL_DUMP`. No gate recipe was changed.
  Sites: `a_real_aomenc_single_ref_gate` (:4256, covers the last2/last3/bwdref/altref2/**altref**
  gates), reference_select :4493, compound_references :4694, obmc :4875, obmc_8x8 :5085,
  warped_motion :5270, interintra :5738, interintra_wedge :5937, free_partitions :6277,
  ab_partitions :6497, masked_compound :6722, cdef :7173, delta_q_and_delta_lf :9502.
- `crates/ec-av1/src/stream.rs:1745` — new `hidden_arnr_arm(name, y4m, args)`: the SECOND ARM.
  Re-encodes the gate's own fixture with the gate's own args **plus** `--arnr-maxframes=0`,
  runs the same decode-order compare, at most once per gate per process. A named
  ("unsupported") refusal on the arm's stream skips the arm; a pixel mismatch is a hard
  failure inside the compare. Called at the same 13 sites.
- `scripts/instrument-aom-oracle.sh` — **rung 13** (coordinator's extra item): lane-inter8 r3's
  hand patch of `~/.cache/aom-oracle/src/av1/decoder/decodemv.c` (an `EC_MODE_MV` line right
  after `assign_mv`, and `stack=<ref_mv_count[...]>` on rung 4's `EC_MODE_VAL`) existed only in
  the build tree; a script rebuild would have dropped it. Both are now script-applied,
  `EC_TRACE_MODE`-gated, idempotent.

## Finding: every gate's own recipe is VACUOUS for hidden frames
`--auto-alt-ref=1 --lag-in-frames=16` does NOT put hidden frames on the wire — libaom's
temporal alt-ref filter absorbs the candidates. Across the whole suite the 15 gates decoded
**281 attempt-streams and saw 5 hidden frames in total**, 13 of the 16 gate names seeing zero.
Every gate claim of "hidden alt-refs included" is propagation only. `--arnr-maxframes=0` as a
second arm fixes it: 13 of 16 arms carry 1-3 hidden frames.

| gate | attempts compared | hidden (own recipe) | ARM `--arnr-maxframes=0` | result |
|---|---|---|---|---|
| reference_select | 1 | 1 | 22 frames, **2 hidden** | pixel-exact |
| compound_references | 1 | 0 | 25 frames, **1 hidden** | pixel-exact |
| obmc | 1 | 0 | 25 frames, **1 hidden** | pixel-exact |
| obmc_8x8 | 0 (gate SKIPs: 74 attempts, 0 8x8-obmc hits) | — | — | not reached (pre-existing vacuity) |
| warped_motion | 40 | 2 (max 1/stream) | 25 frames, **1 hidden** | pixel-exact |
| interintra | 40 | 2 (max 1/stream) | 25 frames, **1 hidden** | pixel-exact |
| interintra_wedge | 40 | 0 | 25 frames, **1 hidden** | pixel-exact |
| free_partitions | 17 | 0 | 25 frames, **1 hidden** | pixel-exact |
| ab_partitions | 17 | 0 | 25 frames, **1 hidden** | pixel-exact |
| masked_compound | 80 | 0 | 27 frames, **3 hidden** | pixel-exact |
| cdef | 40 | 0 | 27 frames, **3 hidden** | pixel-exact |
| an_altref_reference | 1 | 0 | 17 frames, **1 hidden** | pixel-exact |
| delta_q_and_delta_lf | 40 | 0 | 27 frames, **3 hidden** | pixel-exact |
| a_bwdref_reference (bonus, same helper) | 1 | 0 | 18 frames, **2 hidden** | pixel-exact |
| an_altref2_reference (bonus) | 1 | 0 | 64 frames, 0 hidden | vacuous even with the arm |
| a_last2 / a_last3 (bonus, `--auto-alt-ref=0`) | 1 each | 0 | 8 frames, 0 hidden | vacuous by construction (no alt-ref) |
| gradients_with_cdef | n/a | n/a | n/a | single ffmpeg/libaom KEY frame, no alt-ref possible |
| a_golden_reference | n/a | n/a | n/a | `--lag-in-frames=0`, no alt-ref possible |

**No hidden-frame mismatch was found.** Nothing is `#[ignore]`d, no assertion weakened,
no refusal touched (`refusal_inventory.rs` / `gate_coverage.rs` unchanged).

## Commands
    cd /home/tahinli/Documents/Code/Rust/edith_codecs-hidden
    export CARGO_TARGET_DIR=$HOME/.cache/cargo-target-hidden
    EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib -- --nocapture   # table lines: grep HIDDEN
    bash scripts/instrument-aom-oracle.sh                                # rung 13 idempotent

EVIDENCE: /tmp/.../scratchpad/hidden-r2-suite2.log | EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib -- --nocapture (whole suite, 630.72s) | 273 passed, 0 failed, 22 ignored; 281 attempt-streams compared frame-by-frame in decode order, 5 hidden frames on the gates' own recipes, 16 HIDDEN-ARM lines with 21 hidden frames total, every frame pixel-exact
EVIDENCE: same log, HIDDEN-ARM lines | each gate's own args + --arnr-maxframes=0, re-encoded and decoded through decode_all_frames_vs_oracle | 13 of 16 arms carry 1-3 hidden frames; altref2/last2/last3 arms carry 0
EVIDENCE: /tmp/.../scratchpad/rung13/decodemv.c | pristine `git show HEAD:av1/decoder/decodemv.c` + the script's rung-4 then rung-13 python blocks | both probe regions diff-identical to the live hand-patched ~/.cache/aom-oracle/src copy (SAME_MV, SAME_VAL); second application prints "already applied (no-op)"; live aomdec `strings` contains 3 EC_MODE_MV/stack=%d entries

## Residue
- fix-now: none — no defect surfaced.
- deferred: obmc_8x8's hidden compare is unreached because that gate itself never fires an 8x8
  OBMC block (74 attempts, 0 hits) — pre-existing vacuity owned by the OBMC lane; unblocked by a
  recipe that actually codes 8x8 OBMC.
- deferred: `an_altref2_reference` still shows 0 hidden even with the arm (64-frame clip);
  unblocked by a recipe search like the one lanes/altref2-r1.report.md ran for ALTREF2 firing.
- accepted: the arm runs once per gate per process (a full encode + two decodes per arm); the
  gates' own per-attempt sweeps still get the (mostly hidden-free) compare on every attempt.
