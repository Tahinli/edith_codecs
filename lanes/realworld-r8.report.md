VERDICT: Stage 1 partially landed and committed at bd91ad0 (probe + BIT_DEPTH
thread-local + dequant call-site threading). The `u16` widening of
`PlaneBuf`/prediction buffers (item 2 of the charter) was attempted this round,
proved larger than the remaining turn budget, and was REVERTED rather than
left half-wired — this is the charter's own explicitly-sanctioned outcome
("half-wired is the one unacceptable outcome; incomplete-but-named is fine").

## What's landed (bd91ad0, prior round)

- Probed both real films: `Hunger.Games...mkv` -> 3840x1608, `yuv420p10le`,
  HDR10 mastering-display metadata present; `Troy...mkv` -> 1920x792,
  `yuv420p10le`, bt709. Neither ffmpeg header dump showed film grain. Both
  still stop at the `bit_depth != 8` refusal, as expected.
- `BIT_DEPTH` thread-local + `sample_max()` helper (decode.rs), set once per
  frame from the sequence header's real `color_config.bit_depth` in
  `stream.rs`'s decode loop.
- The real `bit_depth` threaded into the `dequant_and_inverse_typed*`/`_wh`
  call sites that hardcoded `8`.
- Refusal is still in place, so behaviour is provably unchanged for every
  existing (8-bit) stream — this increment is inert until PlaneBuf is
  widened and the refusal comes off.

## What was attempted and reverted this round

Widened `mc.rs`'s prediction/reference functions (`sample`, `predict`,
`predict_with_filter(s)`, `predict_compound_intermediate`, `combine_compound`,
`blend_masked_compound`) from `&[u8]`/`&mut [u8]` to `&[u16]`/`&mut [u16]`,
clamped against `crate::decode::sample_max()` instead of the literal `255`,
and made `sample_max()` `pub(crate)` so `mc.rs` could call it. That much is a
clean, self-contained, mechanically-correct sub-step (`mc.rs`'s own unit test
was updated in step with it).

It does NOT compile standing alone, because `PlaneBuf::data` (decode.rs:3425)
— every caller of `mc::predict`/`combine_compound`/etc.'s actual argument
type — is still `Vec<u8>`. Starting the PlaneBuf widen (`data: Vec<u16>`)
immediately fans out: `edges()` returns `(Option<Vec<u8>>, Option<Vec<u8>>,
Option<u8>)` consumed by `intra::predict`, `reconstruct`/`reconstruct_rect`/
`reconstruct_mc`/`reconstruct_mc_rect` all take/write `&[u8]`/`u8` and
`.clamp(0, 255) as u8`, CDEF and ~14 deblock taps do the same, `warp.rs` and
`superres.rs` each have one more clamp-and-cast site, and the final
`PlaneBuf -> Picture` conversion needs an explicit `as u8` narrowing added
back in (Picture's own `y`/`u`/`v` stay `Vec<u8>` in this stage — lifting
that too is stage 2's job, tied to the pixel-format helper `stream.rs`'s own
gate needs). `cargo check -p ec-av1 --lib --tests` on the attempt: **34
library errors + 40 additional test errors**, all `E0308` mismatched-types /
wrong-argument-type from that one root cause (`PlaneBuf::data` untouched)
propagating through every call site, plus 2 `u16` vs `u8` comparison errors
in existing tests.

Given the turn budget remaining when this was discovered (partway through a
55-tool-call-deep session), finishing all ~30+ remaining sites blind risked
landing exactly the state the charter calls out by name as unacceptable — a
compiling-looking but subtly wrong half-widen, or an out-of-budget commit
with the crate not compiling at all. Reverted with `git checkout --
crates/ec-av1/src/decode.rs crates/ec-av1/src/mc.rs`, back to the clean,
green bd91ad0. No uncommitted work lost invisibly: the attempt was stashed
first, popped back, inspected, and only then discarded once its scope was
confirmed too large for the remaining budget — worth stating plainly rather
than silently.

## Exact sites still assuming 8 bits (for the next round)

- `decode.rs:3425` `PlaneBuf::data: Vec<u8>` — the root type, must go first.
- `decode.rs` `PlaneBuf::edges()` (~3472) — returns `(Option<Vec<u8>>,
  Option<Vec<u8>>, Option<u8>)`, consumed by `intra::predict`.
- `decode.rs` `PlaneBuf::reconstruct`/`reconstruct_rect` and
  `reconstruct_mc`/`reconstruct_mc_rect` (~6404 onward) — `prediction: &[u8]`
  params, `.clamp(0, 255) as u8` writes (this is the ~30-site sweep the
  charter named; 22 `Vec<u8>`/`&[u8]`/`&mut [u8]` occurrences counted in
  decode.rs alone this round, plus 4 in `intra.rs`, 1 each in `warp.rs` and
  `superres.rs`).
- CDEF's own reconstruction write and the ~14 deblock taps (decode.rs,
  un-enumerated this round — same `.clamp(0, 255) as u8` pattern, grep it).
- `mc.rs` — DONE this round in the attempt (stashed then reverted along with
  everything else since it doesn't compile alone); the diff is not preserved
  anywhere durable, so the next round redoes this specific file, but it is
  the smallest, most mechanical piece and took under 10 tool calls the first
  time.
- The final `PlaneBuf -> Picture` narrowing point (wherever a finished
  `PlaneBuf` is copied into `Picture.y`/`.u`/`.v`) needs an explicit `as u8`
  added, since `Picture` itself stays 8-bit output this stage.

## Recommendation for the next round

Do the PlaneBuf widen (`data: Vec<u16>`) FIRST, in its own commit-sized step,
before touching `mc.rs` again — the errors it produces are exactly the map
of every remaining site (`cargo check` becomes the sweep's own checklist).
Budget the whole stage 1 as its own round with nothing else assigned, since
one afternoon's attempt (this round, ~20 tool calls) got through `mc.rs`
alone before running short.

## Merge note

No refusal strings changed this round (the bit_depth refusal from r7 is
untouched). `crates/ec-av1/src/decode.rs` and `crates/ec-av1/src/mc.rs` are
both back at their bd91ad0 committed content — `git status` clean, nothing
new to merge from this session beyond bd91ad0 (already committed) and this
report.
