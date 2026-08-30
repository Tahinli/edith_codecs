VERDICT: No commit this round. The `u16` widen of `PlaneBuf::data` was attempted,
got substantially further than r9 (67 -> ~35 remaining `cargo check` errors,
`decode.rs`'s own internal sites plus all of `intra.rs` fully widened and
type-correct on their own), but did not reach a compiling state inside the
turn budget. Per the charter's own rule ("a non-compiling [crate] is not [a
fine place to stop]"), the attempt was reversed with `git apply -R` against a
saved diff rather than committed half-wired. Working tree is clean, at
b8ad140 + the r10 charter commit c7b3fa0, `cargo check -p ec-av1 --lib`
clean, matching the round-start state (262/0 suite, unexercised this round
since nothing changed).

## What was reached this round (and reverted)

`PlaneBuf::data: Vec<u8>` -> `Vec<u16>` (`decode.rs:3428`), `sample_max()`
made `pub(crate)` for cross-module reuse, then the checker-driven sweep:

- `PlaneBuf::edges`/`edges_rect` return types -> `(Option<Vec<u16>>,
  Option<Vec<u16>>, Option<u16>)`.
- `PlaneBuf::reconstruct`/`reconstruct_rect`: prediction buffer `vec![0u16;
  ..]`, both clamp-and-write sites -> `.clamp(0, sample_max()) as u16`.
- `PALETTE_PRED` thread-local widened to `Vec<u16>` (its `colors` source was
  already `u16`, this dropped a stale `as u8` truncation).
- CDEF's single write-back site (`decode.rs:4687`) -> `sample_max()`/`u16`.
- The whole deblock `filter_edge` function (`decode.rs:5121`,
  4/6/8/14-tap taps, ~14 write sites) -> `data: &mut [u16]`, every
  `.clamp(0, 255) as u8` -> `.clamp(0, sample_max()) as u16`.
- Every `PlaneBuf { data: vec![0u8; ...], ... }` zero-init (key frame, inter
  frame, and one more decode entry point — 3 sites) -> `vec![0u16; ...]`.
- Reference-frame ingestion (`Picture.y/u/v` -> `PlaneBuf.data`, 6 struct
  literals: `reference.{y,u,v}` and the per-slot `g.{y,u,v}` loop) ->
  `.iter().map(|&s| s as u16).collect()` (widening in from `Picture`, which
  stays `u8` this stage).
- The two `PlaneBuf -> Picture` narrowing points (both `y/u/v: y.data, ...`
  struct literals) -> `.into_iter().map(|s| s as u8).collect()` (narrowing
  out to `Picture`, which stays `u8` this stage — this is the exact site r8's
  report named as still open, now closed).
- `intra.rs` fully widened end to end: `Edges::build`, `predict`,
  `directional`, `dc`, `predict_filter_intra` all take/return `&[u16]`/
  `Option<u16>`/`&mut [u16]` now, every `.clamp(0, 255) as u8` (5 sites,
  including 2 internal accumulator clamps that were previously untouched)
  -> `.clamp(0, crate::decode::sample_max()) as u16`. Its own `#[cfg(test)]`
  module (`fill`/`checksum`/the two `predict()` call sites) updated in step;
  this file alone type-checked clean in isolation before the wider sweep
  continued.

That diff (500 lines) is saved at
`/tmp/claude-1000/.../scratchpad/r10-attempt.diff` for the next round to
replay rather than redo by hand — it is NOT applied to the tree right now.

## Exact remaining error clusters (the live checklist, ~35 sites when this
round stopped, `cargo check -p ec-av1 --lib` against the PlaneBuf-widened
tree)

- `crates/ec-av1/src/restoration.rs`'s `apply_loop_restoration_plane` still
  takes/returns `Vec<u8>` (`decode.rs:5445/5457/5469`, the 3 call sites that
  assign its result into `y.data`/`u.data`/`v.data`) — untouched this round,
  not inspected past its signature.
- The two remaining debug-dump `write_all(&y.data)` blocks
  (`decode.rs:6165-6167` and `12109-12111`, `EC_AV1_PREFILT_DUMP`) and the
  `crop_wide`/`crop` closures (`6174`-ish and `6209`/`12151`-ish, both build
  a cropped `Vec<u8>` from `plane.data`) still assume `u8` — these are
  debug/superres-crop paths, need the same `.iter().map(|&s| s as u8)`
  narrowing pattern already used at the two `Picture` literals.
- `decode.rs:6520` — `reconstruct_mc`'s own `prediction: &[u8]` param + its
  `.clamp(0, 255) as u8` write, the same pattern as `reconstruct`/
  `reconstruct_rect` above it, not yet touched.
- `decode.rs:6996`/`7071` and `8242` through `10202` (~24 call sites,
  `mc::predict`/`predict_with_filter(s)`/`predict_compound_intermediate`/
  `combine_compound`/`blend_masked_compound`, plus `interintra_blend`'s own
  `pred: &mut [u8]` and its internal `vec![0u8; side*side]` intra buffer at
  `~7040`) — `mc.rs` itself (907 lines, `predict`/`predict_with_filter(s)`/
  `predict_compound_intermediate`/`combine_compound`/
  `blend_masked_compound`, all six still `&[u8]`/`&mut [u8]`) is the one
  big remaining piece; the charter's own note that a prior round widened
  `mc.rs` alone in ~10 tool calls (using the same `sample_max()` pattern
  just proven out in `intra.rs` this round) makes it the right next step —
  do it FIRST next round since PlaneBuf is already committed by then and
  `cargo check` will point at exactly these call sites.
- `crates/ec-av1/src/encode.rs:600/820/872` — three call sites into
  `intra::predict`/`predict_filter_intra` from the ENCODER side, not
  inventoried by r8 or the charter at all (a new site class this round's
  attempt surfaced: the encoder shares `intra.rs` with the decoder, so
  widening `intra.rs` fans out into `encode.rs` too, still on `u8` sample
  buffers there — untouched, not even read this round; next round needs to
  either widen `encode.rs`'s own picture buffers in step or box the u8<->u16
  conversion at those three call sites specifically since the encoder isn't
  in this charter's scope).
- `warp.rs`, `superres.rs` — never reached (errors upstream of them all still
  open); charter's own inventory (1 clamp-and-cast site each) is presumably
  still accurate but unverified this round.

## Why this round stopped short

Turn-budget math: intra.rs's full widen (13 signature/body edits across a
970-line file) and decode.rs's own ~20-site internal sweep both landed
clean and are real, reusable progress — verified by literally getting the
error count from 67 to ~35 with `cargo check` as the checklist, exactly as
charter directed. What remained (`restoration.rs`, `mc.rs`, the newly
discovered `encode.rs` fan-out, two debug-dump closures) was correctly
estimated as too large to land AND leave green before the ~turn-55 stop
point, so the attempt was reversed rather than committed non-compiling.

## Recommendation for r11

1. Re-apply `/tmp/claude-1000/.../scratchpad/r10-attempt.diff` (git apply,
   not by hand) as the starting point — it is the PlaneBuf widen +
   `intra.rs` widen, both proven type-correct in isolation.
2. Fix `restoration.rs`'s `apply_loop_restoration_plane` (small, self
   contained — likely one function).
3. Fix the two debug-dump narrowing closures + `reconstruct_mc`'s clamp
   (small, same pattern as everything already done).
4. Widen `mc.rs` LAST as the charter said, following the exact pattern this
   round proved out in `intra.rs` (`sample_max()`, no other new machinery
   needed) — expect ~24 decode.rs call sites to resolve for free once its
   six functions take `&[u16]`/`&mut [u16]`.
5. New this round: budget a pass over `encode.rs`'s 3 call sites into
   `intra.rs` — decide whether the encoder's own picture buffers widen too
   (likely no, out of this charter's scope) or whether those 3 sites get a
   local u8<->u16 box.
6. Run the 262-test suite before committing (per charter); it was NOT run
   this round since nothing new landed to test.

## Merge note

`git status` clean; nothing to merge. No refusal strings touched. `intra.rs`
and `decode.rs` are byte-identical to c7b3fa0 (`git apply -R` verified with
a clean `cargo check -p ec-av1 --lib`, matching the round-start baseline).
