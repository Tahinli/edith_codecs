# lane-hbd r4 — widen Picture, then lift the refusal

At 7bb2a18. Read `lanes/hbd-r3.report.md` first. r3 landed no code but did the
scoping that makes this round tractable, and it found the real blocker:
`encode::Picture.y/u/v` (`encode.rs:92`) are still `Vec<u8>`, and that type is
shared by the encoder AND by `decode_stream`'s return value and the DPB
reference slots — which are read back for motion compensation on frame 2+, not
just handed to the caller. So the refusal cannot lift honestly until `Picture`
widens.

## Checkpoint policy — this applies to the whole round
r3 got `cargo check` from 61 errors down, ran out of budget, and **reverted**;
so did r10 of the predecessor lane. That is twice the same loss. Do NOT revert a
falling error count. Commit a `wip(hbd):` checkpoint every time the count drops
meaningfully, with the number in the message. A non-compiling commit is fine on
this branch under that prefix; it is never merged in that state. The count
reaching zero is when you run the suite and make a real commit.

Its attempt diff is saved at
`/tmp/claude-1000/-home-tahinli-Documents-Code-Rust-edith-codecs/baaa03f8-c4ff-4469-8ebb-83100429b150/scratchpad/hbd-r3-picture-widen-attempt.diff`
(128 lines, `pad_plane`/`crop_plane` genericized over `T: Copy + Default`, the
two `as u8` narrowing sites at `decode.rs` ~6823 and ~12594 dropped). `git apply`
it first rather than retyping.

r3's measurement of what remains: the PRODUCTION blast radius is small (~10
sites in `encode.rs`, all genericizable or trivial local narrow boxes); the bulk
of the errors are ~50 mechanical `as u8` → `as u16` touch-ups in encoder TEST
FIXTURES. Do the production sites first and checkpoint, then sweep the fixtures.

## Then, in order
1. **Wire `decode::set_bit_depth`.** It exists at `decode.rs:97` and is never
   called — `stream.rs` never passes `seq.color_config.bit_depth` into it, so the
   `BIT_DEPTH` thread-local is dead code defaulting to 8. Everything downstream
   silently assumes 8-bit until this is wired. Wire it, and make sure it is set
   per frame before decode, not once per process.
2. **Narrow refusals for the two paths that are still 8-bit by construction**:
   `film_grain.rs` (grain LUT is `[i32; 256]`) and `superres.rs`
   (`upscale_row` takes `&[u8]` and clamps to 255). Each gets its own named
   `bit_depth != 8` refusal — these are honest, scoped refusals, not the blanket
   one. Declare both in `refusal_inventory.rs`.
3. **Lift the blanket refusal** in `stream.rs`, in the same commit as the gate
   that proves 10-bit decodes.
4. **The 10-bit gate**: `ffmpeg_decode_sequence` hardcodes `-pix_fmt yuv420p`;
   add a second helper emitting `yuv420p10le` with u16 LE parsing, leaving the
   8-bit helper untouched. aomenc needs `--bit-depth=10` and a 10-bit input.
   Confirm from the sequence header that `bit_depth` really is 10 before
   trusting a pixel match, and hard-assert it.
5. **His own films.** Both are `yuv420p10le`: Hunger Games (3840x1608, HDR10,
   Main, no film grain flagged) and Troy (1920x792, bt709, Main, no film grain).
   Extract with `ffmpeg -i <file> -t 3 -c:v copy -f obu <out.obu>` into this
   worktree's `fixtures/`, probe with
   `cargo run -p ec-av1 --example decode_probe`, and record the next refusal
   verbatim — that is the next lane's charter. Never open his media in a GUI app.

8-bit output must stay bit-exact throughout: 264 tests, every real-aomenc gate
among them.

## Budget discipline
75 turns, no reset on resume. At about turn 55, stop starting new work, commit
your checkpoint, and write the report with the current error count in it.

Hard rules: `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-hbd`; foreground
`nice -n 19 cargo ... -j4`; every `cargo test` a timeout >= 600000 ms; ffmpeg
generates bounded with `-t`; `EC_AV1_REQUIRE_AOMENC=1`; aomenc
`--threads=1 --row-mt=0 --sb-size=64`. Sibling worktrees have live agents —
never build in or edit them. Never push, never merge into main; I handle merges.
End with `lanes/hbd-r4.report.md`, VERDICT on line 1.
