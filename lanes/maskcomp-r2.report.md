# lane-maskcomp r2 report

VERDICT: MERGEABLE PARTIAL -- DIFFWTD masked-compound blend ported and gated (hard assert, forbidden refusal, 8/8 hammer green, no pixel mismatches); wedge blend NOT ported, still refused by name (charter's mergeable-partial fallback: "land DIFFWTD alone").

## What landed
- `crates/ec-av1/src/mc.rs`: `diffwtd_mask` (`build_compound_diffwtd_mask`, reconinter.c) and
  `blend_masked_compound` (`aom_lowbd_blend_a64_d16_mask_c`, blend_a64_mask.c), both algebraically
  re-derived onto this crate's unbiased `i32` `predict_compound_intermediate` domain instead of
  libaom's biased CONV_BUF representation -- proved on paper (doc comments) that libaom's
  `round_offset` bias cancels exactly in both functions (`abs(src0-src1)` for the mask, `m + (64-m)
  == 64` for the blend weight sum), so no offset bookkeeping is needed on our side. `round =
  2*FILTER_BITS - round_0 - round_1 + (bd-8)` reduces to the crate's existing `INTER_POST_ROUND`
  (4) for 8-bit content, confirmed against the crate's own `INTER_ROUND_0`/`INTER_ROUND_1_COMPOUND`
  constants (3/7) rather than hand-copied.
- `crates/ec-av1/src/decode.rs`: both `comp_group_idx == 1` sites (`decode_inter_block`'s
  16x16/32x32 leaf, `decode_inter_block8`'s 8x8 leaf) now split on `compound_type`:
  `COMPOUND_WEDGE` (`compound_type == 0`) still refuses by name (wedge mask codebook out of scope
  this round); `COMPOUND_DIFFWTD` (`compound_type == 1`) reads `mask_type` and falls through to
  build+apply the real blend instead of erroring. Real bug fixed while wiring this in:
  `compound_idx` (the distance-weighted-average symbol) was being read unconditionally on
  `!skip_mode && enable_jnt_comp`, missing libaom's `comp_group_idx == 0` outer gate
  (`decodemv.c:1606`) -- dead code before this round (the masked path always errored earlier so it
  never executed with `comp_group_idx == 1` in practice), now gated correctly at both sites.
- `crates/ec-av1/src/stream.rs`: `a_real_aomenc_stream_with_masked_compound_decodes_pixel_exact`
  flipped from r1's soft-skip/expected-refusal shape to r2's hard shape: `masked_compound_hits() >
  0` is now a hard `assert!` (was a soft-skip), a `COMPOUND_DIFFWTD` refusal is now FORBIDDEN
  (asserted against, matching how the interintra gate treats its own ported capability), and a
  `COMPOUND_WEDGE` refusal is still counted as expected/unasserted (r2 scope boundary).

## Verification
- `cargo check -p ec-av1 --release`: clean (only pre-existing missing-doc warnings, same set as
  before this round).
- Gate `a_real_aomenc_stream_with_masked_compound_decodes_pixel_exact`, hammered 8/8 (fresh
  `EC_AV1_GATE_DUMP` self-pin path per run, `/tmp/claude-1000/mc-flake/mc-flake-{1..8}.obu`): all 8
  green, `masked_compound_hits` in {2,3,3,3,4,3,5,3} per run (never zero), zero `COMPOUND_WEDGE`
  refusals fired in any run (content never picked wedge for this synthetic-gradients recipe, so the
  wedge-still-refuses path is untested live this round -- syntax-level only, per r1), zero pixel
  mismatches across all 80-attempt runs x 8 (`23-30 other-capability refusals` [screen-content-tools
  RD flake, pre-existing/documented], `50-64 pixel-exact matches` per run).
- `cargo test -p ec-av1 --release --lib` (full scoped suite): 220 passed, 0 failed, 17 ignored,
  finished in 140.58s -- includes all pre-existing pinned bit-exact regression tests, unaffected.

## Deferred
- deferred: wedge mask codebook (`av1_wedge_params_lookup` + `init_wedge_master_masks`/
  `init_wedge_masks`, reconinter.c) -- explicitly out of r2 per the charter's mergeable-partial
  fallback ("DIFFWTD alone... is mergeable; an unverified wedge table is not"). Unblocks by
  transcribing `wedge_master_oblique_even/odd`/`wedge_master_vertical` + `wedge_codebook_16_*` +
  `wedge_signflip_lookup`, building the 64x64 master masks and cropping/flipping per
  `wedge_index`/`wedge_sign`, verifying a handful of mask rows against a live C-side printf dump
  (per charter, not a hand transcription unchecked), then flipping the `COMPOUND_WEDGE` refusal
  the same way this round flipped `COMPOUND_DIFFWTD`.
- deferred: a live gate run that actually exercises the `COMPOUND_WEDGE`-still-refuses path (to
  prove the r2 gate's own `assert!(msg.contains("COMPOUND_WEDGE"))` line is reachable, not just
  syntactically present) -- unblocks by a higher wedge-yield encoder recipe (aomenc's cpu-used=0 RD
  never picked wedge over diffwtd/plain-average in any of this round's 8x80 = 640 attempts).

## Commit
`feat(av1): port the DIFFWTD masked-compound blend (comp_group_idx==1), wedge still refused` on
branch lane-maskcomp2.
