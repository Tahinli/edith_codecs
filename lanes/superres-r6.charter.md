# lane-superres r6 — use the rungs you built

At a69e1bd. Read `lanes/superres-r5.report.md` if r5 wrote one, else its
commits: `f46e6fb` (EC_AV1_POSTDEBLOCK_DUMP) and `5e511b5`
(EC_AV1_PREFILT_WIDE_DUMP) — r5's deliverable, two oracle rungs, landed. HEAD
also carries r5's uncommitted `decode.rs` work, committed verbatim by the
orchestrator at its cap and **never seen to compile**.

1. `cargo check`, fix whatever r5 left mid-edit, then the full suite. COMMIT the
   moment it is green.
2. Use the rungs. The gate is 2 of 4096 luma pixels off by 1 (frame 2, rows
   17-18, output column 62). r4 exhausted hand-tracing: our arithmetic is
   self-consistent with our own output, so only libaom's ground truth for that
   row can separate "our deblock level/mask is wrong" from "our pre-deblock
   reconstruction is wrong". Dump both, diff at the failing row, fix what it
   localises. COMMIT.
3. Then stage 4: inter-frame superres — scaled-reference MC, spec 7.11.3.3,
   libaom `av1_setup_scale_factors_for_frame` / `av1_convolve_2d_scale`.

Merge note: main is at 53f5358 and has moved a long way (multi-tile key frames,
chroma smooth/paeth, CDEF index reads). If your merge back looks large, say so
in the report and consider `git merge main` here first, resolving it yourself —
you know which side owns each hunk. Main also carries `gate_coverage.rs`,
`refusal_inventory.rs` and `GATES_THAT_SKIP_ON_A_DECODE_ERROR`: report every
refusal string you add, rename or remove, verbatim, and do not write a gate that
turns a decode error into a printed SKIP.

Hard rules: `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-superres`; foreground
`nice -n 19 cargo ... -j4`; every `cargo test` a timeout >= 600000 ms. Sibling
worktrees have live agents — never build in or edit them. Never push, never
merge into `main`. 75-turn cap, does not reset: COMMIT AT EVERY GREEN STEP.
End with `lanes/superres-r6.report.md`, VERDICT on line 1.
