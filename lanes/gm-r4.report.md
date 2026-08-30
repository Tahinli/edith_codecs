VERDICT: FIXED (single-ref global-warp path landed and pin-verified) -- a distinct, narrower, NOT-yet-fixed shape found and safely refused by name, not guess-fixed.

## What was implemented

`allow_warp`'s `global_warp_allowed` branch (`reconinter.c:33-55`), gated INDEPENDENTLY of `motion_mode`, exactly as r3 scoped it:

- `crates/ec-av1/src/warp.rs`: added `pub fn global_warp_params(wmmat: [i32; 6]) -> Option<WarpParams>` -- calls the SAME `get_shear_params` `find_projection` already uses for local `WARPED_CAUSAL`, just keyed on the frame's coded `global_motion[ref]` model instead of a per-block least-squares fit. No second warp implementation.
- `crates/ec-av1/src/decode.rs` (single-ref inter block path, ~line 6391): `if warp_params.is_none() && is_global_mv_block && !force_integer_mv && !gm_ref.invalid { warp_params = crate::warp::global_warp_params(gm_ref.params); }`, placed right after the local-`WARPED_CAUSAL` motion_mode block so `warp_params.is_none()` naturally encodes `allow_warp`'s "local first, else-if global" precedence. `is_global_mv_block` already carries the `min(bw4,bh4) >= 2` (>=8px) size bound `av1_init_warp_params` checks separately.
- `crates/ec-av1/src/stream.rs`: the whole-frame refusal ("any non-IDENTITY global motion for a single-ref frame -> refuse") is GONE. Replaced with two narrower, named refusals for what remains genuinely unverified (see below).

## Pin before/after (`gm-seed43.obu`, `EC_WEDGE_GATE_ATTEMPTS=20 EC_AV1_REQUIRE_AOMENC=1`)

- **Before (r3 baseline):** frame 1 luma MISMATCH, 416 mismatches, first at row 32 col 0 (block `mi=(8,0)`, ROTZOOM on `LAST_FRAME` alone).
- **After:** frame 1 luma **MATCH**. Frames 0-13 all MATCH (all 3 planes, re-confirmed via `scratch_isolate_pinned_mismatch` with `EC_AV1_PIN_N=24`). AB-tested with a temporary `EC_GM_DISABLE` env gate (reverted before commit): disabling the new code reproduces the exact r3 baseline mismatch (416 @ row32/col0) and shows it never resolves across all 24 frames -- confirms the fix, not a coincidence.
- Frame 14 onward hits a **NEW, smaller, distinct** mismatch (441 Y mismatches, worst delta 7, first at row 0 col 18) that the r3 investigation never reached (the whole-frame refusal used to block this frame entirely). Traced (not guessed): the mismatched frame has `reference_select=false` (ruling out the compound-`GLOBAL_GLOBALMV` hypothesis I checked first) and `global_motion = [Identity,Identity,Identity,Rotzoom,Rotzoom,Identity,Identity]` -- i.e. **two different ref slots** (`GOLDEN_FRAME`, `BWDREF_FRAME`) carry an active ROTZOOM model **at the same time**, a shape frame 1's single-active-slot (`LAST_FRAME` alone) case never exercised. Root cause not found within this round's budget; per charter instruction, **not guess-fixed** -- refused by name instead (see below), verified via `scratch_decode_pinned_stream_once` that the refusal now fires cleanly (`Err`, no panic) rather than shipping the wrong pixels.

## Refusal narrowing (stream.rs, `header.frame_type != Key` gate)

1. `gm.model == WarpModel::Affine` (any ref slot) -- AFFINE (6-parameter) models are untested this round; no aomenc fixture in the wedge gate reached one.
2. More than one ref slot concurrently has a ROTZOOM/AFFINE model -- the exact shape frame 14 exposed; only the single-active-slot case is pin-verified.

Proven decodable this round: **ROTZOOM as the sole active non-IDENTITY/non-TRANSLATION global-motion ref slot** (single-ref path). TRANSLATION was already correct (no warp needed, plain `gm_get_motion_vector` translation branch). IDENTITY unaffected (no warp). AFFINE and multi-slot-concurrent-ROTZOOM remain refused.

## Wedge gate (`a_real_aomenc_stream_with_interintra_wedge_decodes_pixel_exact`, `EC_AV1_REQUIRE_AOMENC=1`)

- Before this round (r3): 27 other-capability refusals / 13 matches / wii_hits=3 out of 40.
- After this round: **30 other-capability refusals / 10 matches / wii_hits=4 out of 40**. `ok`. The refusal count rose because several seeds that used to fall through the (now-removed) blanket non-IDENTITY refusal now hit the new, narrower "concurrent ROTZOOM/AFFINE ref slots" refusal by name instead -- correctly refusing an unverified shape rather than attempting (and risking) a decode.

## Gate ladder run this round

- `scratch_isolate_pinned_mismatch` (pin, before/after, `EC_GM_DISABLE` AB test): as above.
- `EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --release --lib a_real_aomenc_stream_with_interintra_wedge -- --nocapture`: `ok`, 30/40 refusals, 10/40 matches, wii_hits=4.
- `EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --release --lib -- --skip a_real_aomenc_stream_with_interintra_wedge`: **227 passed, 0 failed, 17 ignored, 1 filtered** -- unchanged from r3/main baseline. Includes the 14-pin list (last2/last3/bwdref/altref2/altref reference gates, temporal-mvs, obmc/obmc_8x8, interintra, masked-compound, free-partitions, ab-partitions, compound-references, reference_select, warped_motion) -- all `ok`, inert for the IDENTITY-gm pins as expected.

## Files touched
- `crates/ec-av1/src/warp.rs` -- `global_warp_params`, reusing `get_shear_params`.
- `crates/ec-av1/src/decode.rs` -- the `global_warp_allowed` branch wiring in the single-ref inter block path.
- `crates/ec-av1/src/stream.rs` -- whole-frame refusal replaced by the two named refusals above.

## Next round
Root-cause frame 14's mismatch: two concurrently active ROTZOOM ref slots (`GOLDEN_FRAME`, `BWDREF_FRAME`), `reference_select=false` (ruled out as compound), small delta-1 first divergence at row 0 col 18 growing across frames -- same propagating shape class as r3's bug, but NOT the one this round fixed (AB-confirmed via `EC_GM_DISABLE`). Candidates not yet checked: per-ref indexing into `global_motion` when multiple slots are simultaneously non-identity (unlikely, same code path per ref), BWDREF_FRAME-specific reference-buffer/ordering interaction newly exercised now that this class of frame decodes at all (previously wholly refused), or a genuine second warp-related bug distinct from the block-centre `-1` one r3 already fixed. Re-pin with `EC_WEDGE_GATE_ATTEMPTS` to capture a fresh seed whose first mismatch is this exact shape, then range-ladder it the same way r3 did. Compound `GLOBAL_GLOBALMV` warp (the `predict_compound_intermediate`/`combine_compound` call site) is a SEPARATE, still-completely-unwired gap -- only one `warp::warp_affine` call site exists in the file (single-ref); deferred, no evidence yet that any gate fixture reaches it (this round's `reference_select` check on the mismatched frame came back false).
