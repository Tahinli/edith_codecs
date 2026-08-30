# lane-superres r9 — implement the scaled-reference MC r8 derived

At 67f5097, clean, suite green. Read `lanes/superres-r8.report.md` — it is a
finished implementation plan, not a status note. r8 traced spec 7.11.3.3 through
libaom's `scale.c`/`decodeframe.c`/`convolve.c` and proved algebraically that
the algorithm reduces to this crate's existing rounding chain with only the
horizontal-pass sampling grid changing (AV1 superres never scales height, so the
vertical pass is untouched). **No further libaom reading should be needed.**

Its own next-round TODO, in order:
1. The bypass gate FIRST — a real inter frame with `use_superres`, driven below
   `decode_stream` so the refusal cannot short-circuit the code you are
   measuring ([[refusal-short-circuits-its-own-code]]).
   `--superres-denominator` controls non-key frames.
2. `mc::predict_scaled` plus `mc::scale_factor`/`REF_NO_SCALE` per the report's
   pseudocode.
3. Thread `frame_width: usize` through `decode_inter_block`'s 19 call sites —
   r8 confirmed 18 end in `allow_screen_content_tools,` and one at
   `decode.rs:12528` ends in `false,`; check that one first.
4. Scoped refusals at the compound (~7994) and warp/OBMC/interintra (~8670)
   branches, so an unimplemented combination refuses by name rather than
   producing wrong pixels.
5. Only with the gate pixel-exact: lift `stream.rs:217`'s refusal and update
   `refusal_inventory.rs` in the same commit.

Merge `main` (913df61 — loop restoration filters landed since your merge) before
you start, and compile immediately after: three merges in this batch were
text-clean and compile-broken because a newer gate called a decoder whose
signature a lane had changed.

## Budget discipline (I am serious about this)
You have 75 turns and they do NOT reset if you are resumed. At roughly turn 55,
STOP starting new work: commit what is green, and write your report. Five rounds
in the last batch died mid-edit with nothing reported, and each cost a whole
round to recover. A round that ends with an honest report beats one that ends
five turns deeper into an edit.

Hard rules: `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-superres`; foreground
`nice -n 19 cargo ... -j4`; every `cargo test` a timeout >= 600000 ms; fixtures
through `gradients_source(seed, w, h, tail)`; ffmpeg generates bounded with `-t`;
`EC_AV1_REQUIRE_AOMENC=1`; aomenc `--threads=1 --row-mt=0 --sb-size=64`. Oracle
rungs 6, 7, 8, 8b are taken — take 9; env-gated rungs only, the oracle is SHARED.
Sibling worktrees have live agents — never build in or edit them. Never push,
never merge into main. End with `lanes/superres-r9.report.md`, VERDICT on line 1.
