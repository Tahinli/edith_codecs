# lane-hbd r1 — widen the sample type

Fresh worktree off main (71a2624), suite green at 263/0. This lane replaces
lane-realworld, whose decode work is all merged; only the high-bit-depth job is
left, and it now has a clean branch instead of a stale one. Read
`lanes/realworld-r8.report.md` (site inventory) and `lanes/realworld-r10.report.md`
(the previous attempt) first.

**Why this matters:** both AV1 films in the user's library are `yuv420p10le` and
stop at the `bit_depth != 8` refusal. This is the critical path to his own files.

## Start from the saved diff, do not retype it
r10 got `PlaneBuf::data -> Vec<u16>` plus all of `intra.rs` type-correct before
its cap, then reverted, saving the 500-line diff at
`/tmp/claude-1000/-home-tahinli-Documents-Code-Rust-edith-codecs/baaa03f8-c4ff-4469-8ebb-83100429b150/scratchpad/r10-attempt.diff`.
`git apply` it first. If it no longer applies (main moved: UV palette landed),
re-apply what does and redo the rest by hand — but check before assuming.

Then, in this order, per r10's own handoff:
1. `restoration.rs`'s `apply_loop_restoration_plane` (still `Vec<u8>` in/out,
   3 call sites at `decode.rs` ~5445/5457/5469), two debug-dump narrowing
   closures, and `reconstruct_mc`'s clamp — all small and mechanical.
2. `mc.rs` LAST: 6 functions still `&[u8]`/`&mut [u8]`, ~24 `decode.rs` call
   sites resolve for free once they widen. Use the `sample_max()` pattern
   `intra.rs` proved.
3. `encode.rs`'s 3 call sites into `intra.rs` (~600, ~820, ~872) — the encoder
   shares those predictors. r10 found these; they were NOT in the original
   inventory. Decide deliberately: widen the encoder's buffers too, or box
   u8<->u16 locally at those three sites. Say which and why.

**8-bit output must stay bit-exact.** The 263-test suite is the regression gate;
run it before every commit. Do NOT lift the `bit_depth != 8` refusal and do not
build a 10-bit gate this round — `ffmpeg_decode_sequence` hardcodes
`-pix_fmt yuv420p`, so a 10-bit gate needs a second helper with u16 LE parsing,
and that plus the first real 10-bit stream is the next round.

Note from r10: `git checkout` and `git stash` were blocked in that session;
`git apply -R <saved-diff>` is the working revert mechanism if you need one.

## Budget discipline
75 turns, no reset on resume. At about turn 55, stop starting new work: commit
what compiles and passes, and write your report. Do NOT revert working partial
work to make the report tidier — a compiling, green, partially widened crate is
a fine place to stop.

Hard rules: `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-hbd`; foreground
`nice -n 19 cargo ... -j4`; every `cargo test` a timeout >= 600000 ms;
`EC_AV1_REQUIRE_AOMENC=1`. Do NOT open the user's media in a GUI app — ffmpeg
and `decode_probe` only. Sibling worktrees have live agents — never build in or
edit them. Never push, never merge into main; I handle merges. End with
`lanes/hbd-r1.report.md`, VERDICT on line 1.
