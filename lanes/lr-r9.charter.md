# lane-lr r9 — finish what r8 started

At 7ad6d77. Read `lanes/lr-r8.charter.md` (still your charter — r8 never got to
report) and the r7 section of `lanes/lr.report.md`.

r8 stopped at its turn cap having done two things:
1. **Found and fixed a real defect** — `SGR_X_BY_XPLUS1` was mistranscribed at
   `av1_x_by_xplus1[101]` and `[165..=169]`, committed at 7ad6d77. Whether that
   closes the 3110-vs-3109 gap is UNKNOWN to me: the round never reported a
   gate result. **Run the LR gate first and find out** before assuming either
   way — and say plainly in your report which it was.
2. Started a merge of an already-stale `main`. I aborted it; nothing of yours
   was lost. Redo it against current `main` (a8724cb, far ahead: tile rows,
   delta_q/delta_lf, key-frame superres, palette-Y, a `bit_depth != 8` refusal,
   and two guard tests that will fail until this branch's refusal list matches).

If the gate is still red, the r8 charter's method stands and has not been tried:
decode the ONE pinned stream `fixtures/lr-sgr-r7.obu` (seed 46, `EC_AV1_GATE_DUMP`
already wired at `stream.rs` ~4791) instead of the 40-attempt sweep, with a dump
keyed CALL-UNIQUELY to the failing call — r6's coordinate-keyed dump
(`v_start == 60`) captured some other invocation and cost a whole round
([[dump-not-call-unique]]). `lr_sample`, the box-sum formula and the fast arm are
independently reconfirmed correct; do not re-suspect them.

Note that r8's find is itself the lesson: a mistranscribed constant in a 256-entry
table is invisible to every by-hand review and shows up as a one-off arithmetic
gap. If the gate is still red, diff the OTHER SGR tables against libaom
wholesale — `av1_one_by_x`, the `sgr_params` rows — before tracing anything.

Merge `main` into this branch, resolve it yourself, and land the round green so
the SGR fix can reach main either way: a correct table fix is worth merging even
if the last pixel is still out, provided the gate's status is stated honestly.

Hard rules: `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-lr`; foreground
`nice -n 19 cargo ... -j4`; every `cargo test` a timeout >= 600000 ms;
`EC_AV1_REQUIRE_AOMENC=1`. The oracle at `~/.cache/aom-oracle` is SHARED with
five sibling lanes — instrument only via env-gated rungs in
`scripts/instrument-aom-oracle.sh`, never a throwaway patch left in the tree, and
note that rung numbers 6, 7, 8 and 8b are already taken on main; take 9. Sibling
worktrees have live agents — never build in or edit them. Never push, never merge
this branch into main. 75-turn cap, does not reset: COMMIT AT EVERY GREEN STEP.
End with `lanes/lr-r9.report.md`, VERDICT on line 1.
