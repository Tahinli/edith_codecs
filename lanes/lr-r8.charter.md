# lane-lr r8 — one call, not forty

At 1c122a2. Read the r7 section of `lanes/lr.report.md` first.

r7's real result is a RETRACTION, and it is the most useful thing in the lane:
r6's recorded divergence — dense-arm "u" tap (A,B) = (254,303) vs our 3110 —
**could not be reproduced**. Compiled libaom's own `calculate_intermediate_result`
(now exported by rung 6 of `scripts/instrument-aom-oracle.sh`) gives (253,454) at
that pixel; our `compute_ab`/`lr_sample` live-trace reads the correct raw bytes
(111/154/194); and an independent Python replica of the box-sum formula fed those
bytes reproduces libaom's (253,454) exactly. So `lr_sample`, the box-sum formula
and the fast arm are all reconfirmed correct — **do not re-suspect any of them.**

The reason r6's number was wrong is the round's real finding: its dump matched on
a COORDINATE (`v_start == 60`) that is not unique across the gate's 5-attempt
sweep, so the nine bytes it captured are not provably from the failing call.
A measurement that cannot name which call produced it is not evidence.

## The job
1. Decode the ONE pinned stream, not the sweep: `fixtures/lr-sgr-r7.obu` is
   seed-46's mismatching stream (gitignored, worktree-local; regenerate with
   `EC_AV1_GATE_DUMP=fixtures/lr-sgr-r7.obu EC_LR_GATE_ATTEMPTS=10` if lost).
   `EC_AV1_GATE_DUMP` is already wired into the gate (`stream.rs` ~4791).
2. Re-add a debug dump keyed CALL-UNIQUELY to that single call — r7 suggests
   gating on `xqd == [-16,-32]` — and get the real nine-byte window it reads.
   Compare against `100,111,117,140,154,163,182,194,201`.
3. With a call-unique measurement in hand, either the bytes differ (a sampling /
   stripe-boundary defect, and you have the coordinates) or they match and the
   3110-vs-3109 gap lives downstream of `compute_ab`. Follow whichever the
   measurement says; do not carry r6's hypothesis forward.

Then merge `main` into this branch and resolve it yourself — main has moved a
long way (multi-tile, delta_q/delta_lf, a `bit_depth != 8` refusal) — so the lane
arrives mergeable. And check `refusal_inventory.rs` still matches this branch's
refusal set.

Oracle hygiene: `~/.cache/aom-oracle` is SHARED with five sibling lanes. Rung 6
is permanent and env-safe; any further change goes in as a rung in
`scripts/instrument-aom-oracle.sh` (silent when unset, idempotent), never as a
throwaway patch left in the shared tree. Rebuild with
`ninja -C ~/.cache/aom-oracle/build libaom.a`.

Hard rules: `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-lr`; foreground
`nice -n 19 cargo ... -j4`; every `cargo test` a timeout >= 600000 ms;
`EC_AV1_REQUIRE_AOMENC=1`. Sibling worktrees have live agents — never build in or
edit them. Never push, never merge this branch into main. 75-turn cap, does not
reset: COMMIT AT EVERY GREEN STEP. End with `lanes/lr-r8.report.md`, VERDICT on
line 1.
