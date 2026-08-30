# lane-lr r7 — one tap

At e5ad671 (+ this charter). Read the r6 section of `lanes/lr.report.md`.

## Where you stand — the search is down to one number
r6 linked libaom's real `av1_apply_selfguided_restoration_c` out of
`~/.cache/aom-oracle/build/libaom.a` and called it directly on OUR OWN
`lr_sample`-substituted bytes. It reproduced 194. That decisively rules out
`lr_sample` and the stripe boundary — r5's suspicion — and it rules out the
fast (`r0`) arm, whose `flt0` is bit-exact against real libaom.

What is left: the dense (`r1`) arm. Eight of its nine A/B taps at the failing
pixel match a from-scratch box-sum of the real bytes; the combined `flt1` is
**3110 where libaom gets 3109**. One tap, or a rounding mode.

r6's harness is now committed at `scripts/lr-sgr-pin-harness.c` — it lived in
the scratchpad, which this box reaps (class `oracle-in-reaped-dir`). Use and
extend it.

## Do not
- Re-suspect `lr_sample` or the stripe boundary. Proven correct.
- Re-suspect the fast arm. Proven bit-exact.
- Hand-trace further. Three rounds have now shown our arithmetic is
  self-consistent with our own output; only libaom's own intermediate values
  can break the tie.

## Do
r6 named the two ways to get per-tap ground truth, and preferred the first:
1. `calculate_intermediate_result` and `boxsum1` are `static` in libaom's
   `restoration.c`, so they cannot be called from the harness as-is. Patch them
   non-static in the oracle copy and rebuild (~5 min) — the same pattern
   `scripts/instrument-aom-oracle.sh` already uses for the trace rungs, so add
   it there as an env-gated rung rather than an ad-hoc edit, and it stays
   reusable.
2. Or bisect single input pixels through the existing harness until the tap
   that diverges identifies itself.

Then fix it, get the gate green, COMMIT immediately.

## Then
Wiener and self-guided are otherwise feature-complete with the refusal already
dropped, so once green this lane is ready to merge. Before it does: confirm
`gate_coverage.rs`'s `NEVER_EXERCISED` no longer needs `enable-restoration`
(main's copy — down to `enable-intrabc` alone as of this batch), and consider
`git merge main` here first and resolving it yourself; main is at 93d9510 and
has landed multi-tile decode, CDEF index reads, chroma modes, a `bit_depth != 8`
refusal and three reworded partition refusals since this branch forked. Report
every refusal string you add, rename or remove, verbatim.

Hard rules: `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-lr`; foreground
`nice -n 19 cargo ... -j4`; every `cargo test` a timeout >= 600000 ms. Sibling
worktrees have live agents — never build in or edit them. Never push, never
merge into `main`. 75-turn cap, does not reset: COMMIT AT EVERY GREEN STEP, and
commit any harness or instrument you build — a scratchpad is not storage.
Update `lanes/lr.report.md` with an r7 section, VERDICT first.
