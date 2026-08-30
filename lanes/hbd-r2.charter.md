# lane-hbd r2 — 42 errors to zero

At a86872b. r1 applied the saved diff and widened well past it before its cap;
I committed its tree verbatim. **It does not compile: `cargo check -p ec-av1
--lib` reports 42 errors, 41 of them E0308 type mismatches.** There are no
conflict markers — I checked.

That number is your progress metric. Drive it to zero.

## Checkpoint policy for this round — read this, it is different
A half-widened crate cannot compile, so "commit only when green" would mean
committing nothing until the very end, which is exactly how two rounds have died
here. Instead: **commit a `wip(hbd):` checkpoint whenever the error count drops
meaningfully** (say every 10 errors), with the count in the message. A
non-compiling commit is acceptable ONLY on this branch, ONLY with that prefix,
and ONLY while the count is falling — it is never merged in that state. Once the
count hits zero, run the full suite and make a real commit.

## Order (r10's, still valid)
`restoration.rs`'s `apply_loop_restoration_plane` and its 3 `decode.rs` call
sites; the debug-dump narrowing closures; `reconstruct_mc`'s clamp; then `mc.rs`
LAST — its 6 `&[u8]`/`&mut [u8]` functions unlock ~24 `decode.rs` call sites at
once, using the `sample_max()` pattern `intra.rs` already proves. Then
`encode.rs`'s 3 call sites into `intra.rs`: decide deliberately whether the
encoder's buffers widen too or get a local u8<->u16 box, and say which and why —
the encoder shares those predictors and was not in the original inventory.

**8-bit output must stay bit-exact.** The 263-test suite is the gate once it
compiles; a single pixel of drift there means a clamp bound or a rounding step
was widened wrong.

Do NOT lift the `bit_depth != 8` refusal and do not build a 10-bit gate — that
is the next round, and it needs a second `ffmpeg_decode_sequence` helper emitting
`yuv420p10le` with u16 LE parsing.

`git checkout` and `git stash` have been blocked in these sessions; use
`git apply -R <diff>` if you need to undo something.

## Budget discipline
75 turns, no reset on resume. At about turn 55, stop starting new work, commit
your checkpoint, and write the report with the current error count in it.

Hard rules: `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-hbd`; foreground
`nice -n 19 cargo ... -j4`; every `cargo test` a timeout >= 600000 ms;
`EC_AV1_REQUIRE_AOMENC=1`. Do NOT open the user's media in a GUI app. Sibling
worktrees have live agents — never build in or edit them. Never push, never merge
into main; I handle merges. End with `lanes/hbd-r2.report.md`, VERDICT on line 1.
