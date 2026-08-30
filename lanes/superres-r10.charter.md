# lane-superres r10 — the bypass gate, then the lift

At fee8ba3. Read `lanes/superres-r9.report.md`. The plumbing exists:
`mc::predict_scaled`/`scale_factor`/`REF_NO_SCALE` are landed and pinned
bit-exact against `predict_with_filters` at `REF_NO_SCALE`, and
`decode_inter_block` carries `frame_width` through all 19 call sites. Three
scoped refusals cover the combinations without scaled-MC support (compound,
warp/OBMC/interintra, `decode_inter_block8`'s 8x8 leaf).

**But none of that code is reachable yet**, because `stream.rs:217` still
refuses `use_superres` on an inter frame at the frame level. Unreachable code is
unproven code — that is this round's whole point.

## Order
1. Merge `main` (170a5a3) and compile immediately.
2. The bypass gate: a real aomenc stream with a key frame and an inter frame,
   both with `use_superres` (`--superres-denominator` controls non-key frames,
   `--superres-kf-denominator` the key ones), driven below `decode_stream` so
   the frame-level refusal cannot short-circuit it
   ([[refusal-short-circuits-its-own-code]]). Hard-assert that the scaled path
   actually fired — a hit counter on `predict_scaled`, not just a pixel match,
   since an unscaled reference would pass the pixels and prove nothing
   ([[gate-blind-to-feature]]).
3. Pixel-exact vs ffmpeg, then lift `stream.rs:217`'s refusal and update
   `refusal_inventory.rs` in the same commit.
4. If the gate mismatches, bisect against the oracle rather than by inspection.
   Rungs 6, 7, 8, 8b are taken; 9 and 10 are spoken for by sibling lanes — take
   11.

`decode_inter_block8`'s blanket refusal can stay blanket this round; narrowing
it is worth its own round and is not what stands between this lane and main.

## Budget discipline
75 turns, and they do NOT reset if you are resumed. At about turn 55, stop
starting new work: commit what is green and write your report.

Hard rules: `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-superres`; foreground
`nice -n 19 cargo ... -j4`; every `cargo test` a timeout >= 600000 ms; fixtures
through `gradients_source(seed, w, h, tail)`; ffmpeg generates bounded with
`-t`; `EC_AV1_REQUIRE_AOMENC=1`; aomenc `--threads=1 --row-mt=0 --sb-size=64`.
The oracle is SHARED — env-gated rungs only. Sibling worktrees have live agents —
never build in or edit them. Never push, never merge into main. End with
`lanes/superres-r10.report.md`, VERDICT on line 1.
