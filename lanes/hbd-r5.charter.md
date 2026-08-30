# lane-hbd r5 — steps 1 to 5, nothing to widen

At main-merged HEAD. **The Picture widen is DONE and merged** (main fff022d,
264/0, workspace clean). Do not re-attempt it. r4 never got to write its report
to disk — its content is in `lanes/hbd-r4.report.md` if a later hand copied it;
if that file is missing, do not spend turns reconstructing it, the facts you
need are here.

I also fixed one thing r4 could not see: it checked `-p ec-av1` only, and
`tools/ec-bench` builds a `Picture` from 8-bit y4m. Widened at the call site.
Check `--workspace --all-targets`, not just this crate, before you call a
compile clean.

## The round: charter steps 1-5, in order, each committed
1. **Wire `decode::set_bit_depth`.** It is at `decode.rs:97`, never called, so
   the `BIT_DEPTH` thread-local is dead code defaulting to 8 — the compiler says
   so (`function set_bit_depth is never used`). Call it from `stream.rs` with
   `seq.color_config.bit_depth`, per frame before decode, not once per process.
   Everything downstream silently assumes 8 until this lands.
2. **Narrow refusals** for the two paths that are 8-bit by construction:
   `film_grain.rs` (grain LUT is `[i32; 256]`) and `superres.rs` (`upscale_row`
   is `&[u8]`, clamps at 255). Each gets its own named `bit_depth != 8` refusal,
   declared in `refusal_inventory.rs`. These are honest scoped refusals; the
   blanket one is not.
3. **The 10-bit gate.** `ffmpeg_decode_sequence` hardcodes `-pix_fmt yuv420p`;
   add a second helper emitting `yuv420p10le` with u16 LE parsing, leaving the
   8-bit helper untouched — every existing gate depends on it. aomenc needs
   `--bit-depth=10` and a 10-bit input. Confirm from the sequence header that
   `bit_depth` really is 10 before trusting a pixel match, and hard-assert it.
4. **Lift the blanket refusal** in `stream.rs`, in the SAME commit as that gate,
   updating `refusal_inventory.rs`.
5. **His own films.** Both `yuv420p10le`: Hunger Games (3840x1608, HDR10, Main,
   no film grain flagged) and Troy (1920x792, bt709, Main, no film grain).
   Extract into this worktree's `fixtures/` with
   `ffmpeg -i <file> -t 3 -c:v copy -f obu <out.obu>` (note `-f obu`), probe with
   `cargo run -p ec-av1 --example decode_probe -- <out.obu>`, and record the next
   refusal VERBATIM with the film and header state. That sentence is the next
   lane's charter. Never open his media in a GUI app.

Expect 10-bit to expose rounding that 8-bit hid — the inverse transform's
intermediate clamps already take `bit_depth`, but anything still assuming a 255
ceiling shows up as a small pixel delta, not a crash. Bisect against the oracle,
not by inspection.

If a step does not fit, commit the ones that do and say which. Landing red on
this branch is fine; only merging red is forbidden, and I do the merges.

## Budget discipline
75 turns, no reset on resume. At about turn 55, stop starting new work: commit,
and write `lanes/hbd-r5.report.md` — r4 hit the cap before it could write its
report at all, which is the one artifact I cannot reconstruct.

Hard rules: `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-hbd`; foreground
`nice -n 19 cargo ... -j4`; every `cargo test` a timeout >= 600000 ms; ffmpeg
generates bounded with `-t`; `EC_AV1_REQUIRE_AOMENC=1`; aomenc
`--threads=1 --row-mt=0 --sb-size=64`. Sibling worktrees have live agents —
never build in or edit them. Never push, never merge into main.
