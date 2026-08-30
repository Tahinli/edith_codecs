# lane-hbd r3 — the payoff: decode a 10-bit stream

At main (8440024). The widen is MERGED: the decoder's sample type is `u16`
everywhere, clamp bounds come from `bit_depth`, 8-bit output is bit-exact at
264/0, and the encoder keeps `u8` with a local box at its four call sites.

Everything now stands behind one refusal: `"a stream whose bit depth is not 8
(this decoder reconstructs into 8-bit planes)"` in `stream.rs`. The sentence is
already false about the planes. Lift it — properly.

## Order
1. **A 10-bit reference path.** `ffmpeg_decode_sequence` (`stream.rs`) hardcodes
   `-pix_fmt yuv420p`, one byte per sample. Add a second helper emitting
   `yuv420p10le` and parsing u16 little-endian, and make the comparison
   bit-depth aware. Keep the 8-bit helper exactly as it is — every existing gate
   depends on it.
2. **A gate on a real 10-bit aomenc stream.** `aomenc` needs
   `--bit-depth=10 --profile=0` and a 10-bit input (`ffmpeg ... -pix_fmt
   yuv420p10le`); confirm from the sequence header that `bit_depth` really is 10
   before trusting any pixel match. Hard-assert it.
3. **Lift the refusal** and update `refusal_inventory.rs` in the SAME commit —
   only once that gate is pixel-exact.
4. **Then the real target: his own files.** Both AV1 films on this box are
   `yuv420p10le`:
   `~/Downloads/The.Hunger.Games...2160p.AV1.HDR10...mkv` and
   `~/Videos/Films/Troy...1080P.AV1...mkv`. Extract a few seconds
   (`ffmpeg -i <file> -t 3 -c:v copy -f obu <out.obu>`, note `-f obu`) and run
   `cargo run -p ec-av1 --example decode_probe -- <out.obu>`. Whatever refusal
   comes back next IS the next lane's charter — record it verbatim, with the
   film and the header state. Do not open his media in a GUI app; ffmpeg and
   decode_probe only, and write extracted streams into this worktree's
   `fixtures/`, never beside his files.

Expect 10-bit to expose rounding that 8-bit hid: the inverse transform's
intermediate clamps already take `bit_depth` (`row_clamp = bit_depth + 8`,
`col_clamp = max(bit_depth + 6, 16)`), but every place that still assumes a
255 ceiling or an 8-bit rounding constant will show up as a small pixel delta,
not a crash. Bisect any mismatch against the oracle rather than by inspection.

## Budget discipline
75 turns, no reset on resume. At about turn 55, stop starting new work: commit
what is green and write your report.

Hard rules: `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-hbd`; foreground
`nice -n 19 cargo ... -j4`; every `cargo test` a timeout >= 600000 ms; ffmpeg
generates bounded with `-t`; `EC_AV1_REQUIRE_AOMENC=1`; aomenc
`--threads=1 --row-mt=0 --sb-size=64`. Sibling worktrees have live agents —
never build in or edit them. Never push, never merge into main; I handle merges.
End with `lanes/hbd-r3.report.md`, VERDICT on line 1.
