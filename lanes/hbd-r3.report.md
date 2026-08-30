# lane-hbd r3 report

VERDICT: NO CHANGE MERGEABLE -- scoping round only, tree left clean at 8281618, next lane's charter attached below.

## What happened

This round budgeted 75 turns for "the payoff": a 10-bit reference path, a
real 10-bit aomenc gate, lifting the `bit_depth != 8` refusal, then probing
the user's own films. Most of the budget went to establishing that the
refusal cannot be honestly lifted without widening `encode::Picture` itself
-- a materially bigger job than the charter's framing suggested -- and a
first attempt at that widen, which does not fit in what was left. No commit
was made; `git diff` is empty, worktree is at 8281618 exactly.

## The real blocker (not visible from the charter alone)

r2 widened every *intermediate* sample buffer (`PlaneBuf`, `mc.rs`,
`warp.rs`, the OBMC/interintra chains) to `u16`, but the FINAL narrowing --
where a decoded frame becomes an `encode::Picture` -- was untouched, and
`Picture.y/u/v` are still `Vec<u8>`. Two places do `s as u8` on the way out
(`decode.rs:6823-6825` for key frames, `decode.rs:12594-12596` for inter
frames). Widening only the ffmpeg comparison helper (charter step 1) proves
nothing on its own: even with a correct 10-bit-precision decode internally,
the value that reaches the caller is still an 8-bit truncation, which is
exactly the "silent wrongness" the refusal's own doc names. So "lift the
refusal properly" (step 3) requires `Picture` to actually carry 10-bit
values, not just the reference path to be able to read them.

`encode::Picture` is shared between the encoder (its own reconstruction
output, `u8`-only by design per r2's explicit decision) and the decoder
(`decode_stream`'s return type, and the DPB's own `ref_slots` storage used
for inter-frame motion compensation -- `reference: &Picture` is read for MC,
not just written at the end, so reference-frame precision matters for
correctness on frame 2+, not only for the final comparison). Widening
`Picture.y/u/v` to `Vec<u16>` is the natural single-source-of-truth fix and
turned out to have a SMALLER encoder blast radius than first feared --
production encoder code only touches `Picture` at ~10 sites (`Plane`
construction, `pad_plane`/`crop_plane`, `padded_to`, `crop_encoded`), all
either genericizable over the element type or a trivial local `as u8`
narrow (the encoder never needs anything but `0..=255`). The real size was
in test-fixture churn: `cargo check --lib` alone showed 61 errors, dominated
by ~50 encoder test-module literal-assignment sites (`picture.y[i] = ... as
u8`) that all need `as u16`, plus the two decode.rs narrowing sites, plus
whatever `--tests` adds on top (not reached this round). `film_grain.rs` and
`superres.rs` are ADDITIONALLY hardcoded to 8-bit math (grain's scaling LUT
is `[i32; 256]`, its doc says "specialized to this crate's only picture
shape: 8-bit"; superres's `upscale_row` is `&[u8]` with a literal
`.clamp(0, 255) as u8`) -- neither is spec-generalizable to 10-bit inside
this round's remaining budget, so the honest plan was to lift the BLANKET
`bit_depth != 8` refusal but add two NARROWER ones ("film grain on a stream
whose bit depth isn't 8", "use_superres on a stream whose bit depth isn't
8") guarding those two subsystems specifically, since the core
transform/prediction/MC/deblock/CDEF/LR pipeline is otherwise already
bit-depth-generic via `decode::BIT_DEPTH` (which is itself dead code right
now -- `set_bit_depth` is defined but never called from `stream.rs`; wiring
that is also part of the real fix).

A first attempt at the `Picture` widen was made (`Picture.y/u/v: Vec<u16>`,
`pad_plane`/`crop_plane` genericized over `T: Copy [+ Default]`, the two
decode.rs narrowing sites changed to move `PlaneBuf::data` directly instead
of `.map(|s| s as u8)`) and got `cargo check -p ec-av1 --lib` down from 61
to a smaller but still nonzero error count before the turn budget forced a
stop; the attempt was reverted (`git checkout --`) rather than left
half-compiling, so the tree stays green at 8281618. The diff is saved for
reference at
`/home/tahinli/Documents/Code/Rust/edith_codecs-hbd/../..` -- actually at
the session scratchpad: `hbd-r3-picture-widen-attempt.diff` (128 lines,
2 files: `decode.rs`, `encode.rs`). It is NOT applied to the worktree.

## What was NOT reached

Steps 1 (ffmpeg 10-bit helper), 2 (real 10-bit aomenc gate), 3 (lift +
inventory update), 4 (probe his films) -- none started. No fixture files
were extracted, nothing was written to `fixtures/`.

## Next lane's charter (hbd-r4)

1. Apply (or redo, it's small and now-scoped) the `Picture.y/u/v ->
   Vec<u16>` widen: `encode.rs` struct + `pad_plane`/`crop_plane`
   genericized (both already attempted, low-risk) + ~10 production
   `Plane`-construction/padding call sites (local `as u8`/`as u16` box,
   same pattern as r2's `intra_predict_u8`) + the ~50 encoder test-fixture
   literal touch-ups (mechanical, `cargo check --lib` then `--tests`
   enumerates every one) + `decode.rs`'s two narrowing sites (drop the `as
   u8` map, move `PlaneBuf::data` directly).
2. Wire `decode::set_bit_depth(seq.color_config.bit_depth)` in `stream.rs`
   before the per-frame decode calls -- currently dead code, defaults to 8
   always.
3. Add two narrow refusals (`film_grain.rs`'s `apply_grain` and
   `header.use_superres`, both gated on `bit_depth != 8`) rather than
   generalizing their 8-bit-hardcoded math this round -- name them
   explicitly in `refusal_inventory.rs`.
4. Lift the blanket `bit_depth != 8` refusal in `stream.rs` once (1)-(3)
   compile and the existing 264-test 8-bit suite is still green (it should
   be structurally: `Vec<u16>` holding `0..=255` compares byte-for-byte
   equal in value to the old `Vec<u8>`, only the comparison TYPE changes).
5. THEN do the charter's original steps 1/2/3/4: 10-bit ffmpeg reference
   helper (u16 LE parse), real `--bit-depth=10 --profile=0` aomenc gate
   hard-asserting `bit_depth == 10` from the sequence header before
   trusting any pixel match, and only then probe the user's two real films
   (`~/Downloads/The.Hunger.Games...2160p.AV1.HDR10...mkv`,
   `~/Videos/Films/Troy...1080P.AV1...mkv`) via `ffmpeg -t 3 -c:v copy -f
   obu` into this worktree's `fixtures/` + `decode_probe`. Given both films
   are HDR10 (film grain is common in HDR masters), expect the probe to hit
   the new film-grain-at-10-bit refusal quickly -- record it verbatim with
   header state as instructed, that becomes hbd-r5.

## Budget

Spent essentially the whole 75-turn budget on: reading the charter/r2
report, tracing the full narrowing chain (`Picture` shared by encoder+DPB,
not just a comparison-helper concern), sizing the blast radius across
encode.rs/decode.rs/film_grain.rs/superres.rs, and one partial widen
attempt that was reverted rather than left broken. This was a scoping
failure on my part, not a budget-external blocker: the charter's own
"expect 10-bit to expose rounding" framing undersold that the OUTPUT type
itself, not just intermediate clamps, needed widening -- that should have
been the very first thing checked (grep `Picture.*Vec<u8>`) before reading
deeply into the rest of the pipeline.
