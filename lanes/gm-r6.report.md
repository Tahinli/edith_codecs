VERDICT: FIXED. Root cause: the single-reference mv-stack predictor (`mvstack.rs`
`find_mv_stack_with_sign_bias`) fell back to `(0, 0)` when the real neighbour scan came up
short of `MAX_MV_REF_CANDIDATES` (2) entries, instead of libaom's real fallback -- this
querying block's OWN global motion vector for its ref frame (`gm_get_motion_vector`, i.e.
`gm_mv(gm, ref_frame)`). The bug was invisible for every stream tested before this round
because a live ref's global motion was always identity/translation there (whose gm mv is
itself `(0,0)`), and only became visible once a ref carried a real ROTZOOM/AFFINE model *and*
a querying block's own stack came up short -- frame 14's `mi=(0,0)`, the very first block
decoded in the frame, has zero neighbours (`max_row_offset`/`max_col_offset` both 0), so its
entire predictor was the fallback.

## Range-ladder evidence

Built the missing oracle instrument last round asked for: `~/.cache/aom-oracle`'s
`decodemv.c` `read_inter_block_mode_info` now prints `EC_MODE mi_row mi_col rng=` before, and
`EC_MODE_VAL ... mode ref0 ref1 mv0 rng=` after, gated `EC_TRACE_MODE=1`. Ran it directly on
the recaptured pin (`/tmp/claude-1000/gm-r5-pin.obu`, still present from r5, re-verified
byte-identical decode twice via `EC_AV1_PIN`/`scratch_isolate_pinned_mismatch`):

- Frame 14's `mi=(0,0)` group (14th inter-frame group in trace order, frame0 is intra/untraced):
  `EC_MODE mi_row=0 mi_col=0 rng=34391` ... `EC_MODE_VAL mi_row=0 mi_col=0 mode=16 ref0=4
  ref1=-1 mv0=(4,4) rng=46688` (mode 16 = `NEWMV`, ref 4 = `GOLDEN_FRAME` -- matches r5's own
  finding exactly).
- Our own decoder, same pin, `EC_AV1_TELL=1` (block-entry/post-* range trace already wired):
  `post_is_inter ... range=34391` (matches oracle's entry `rng` **exactly**) ... `post_interp_filter
  ... range=46688` (matches oracle's exit `rng` **exactly**). Every symbol between mode/ref/mv-joint/
  mv-component/interp-filter reads the identical number of bits in the identical order --
  **symbol consumption is bit-exact**, ruling out an entropy-side (CDF/context) defect.
- But `EC_AV1_TRACE`'s own decoded value at that same point (`tell=35`, matching
  `post_interp_filter`'s `tell=35`): `mv=(-6,-4)` -- **not** `(4,4)`. Same bits read, different
  final value: the defect is in *prediction-side construction* (the predictor a correctly-read
  delta gets added to), settling r5's open prediction-vs-entropy question decisively in favour
  of prediction.
- Confirmed against libaom source (`mvref_common.c` `setup_ref_mv_list`'s single-reference-
  extension tail, ~line 778): `for (idx = *refmv_count; idx < MAX_MV_REF_CANDIDATES; ++idx)
  mv_ref_list[idx] = gm_mv_candidates[0]` -- the GM fallback, unclamped, filled *after* the real
  entries are clamped. `decodemv.c`'s `read_inter_block_mode_info_impl` then reads `nearestmv[0]`
  from exactly this array (`av1_find_best_ref_mvs(..., ref_mvs[ref_frame[0]], &nearestmv[0], ...)`)
  for the non-compound, non-GLOBALMV case -- which a `refmv_count == 1`/`NEWMV` block still uses
  as its base predictor (`ref_mv[0]` stays `nearestmv[0]` unless `ref_mv_count > 1`).

## Fix

`crates/ec-av1/src/mvstack.rs`, `find_mv_stack_with_sign_bias`: `nearest_mv`/`near_mv` now
default to `gm_mv(gm, ref_frame)` (this block's own ref's global-motion vector, already
computed once per block by `build_gm_mv_table` in `decode.rs` and threaded in as `gm`) instead
of `(0, 0)` when the corresponding candidate slot is missing; `pred_mv` is just `nearest_mv`
now (the old `candidates.is_empty()` special case collapsed into the same fallback). Only the
single-reference path touched -- `find_mv_stack_compound` (the two-ref stack) is untouched and
stays out of scope; compound blocks are still refused separately by `reference_select`.

Re-ran `scratch_isolate_pinned_mismatch` on the r5/r6 pin with the multi-slot refusal
temporarily neutralized (same recipe as r5's step 1): all 24 frames, all 3 planes, MATCH --
where r5 had 441/576 mismatching pixels on frames 14/15, this round has zero. Then reverted
the neutralization and re-ran the *unmodified* `decode_stream` on the same pin: still all 24
frames byte-exact, proving the fix holds through the real entry path, not just the bypassed one.

## Refusal lifted

The "more than one concurrently active ROTZOOM/AFFINE ref slot" refusal in `stream.rs` is
**removed** -- it was masking this predictor bug, not naming an unimplemented capability; the
AFFINE-on-single-ref-frame refusal right above it is untouched (still genuinely unverified,
per r5/r4).

## Refusal count (wedge gate, `EC_WEDGE_GATE_ATTEMPTS=40 EC_AV1_REQUIRE_AOMENC=1`)

- Before (r4's last measurement): 30/40 named refusals (20 of them the multi-slot-ROTZOOM
  string this round lifted), 10/40 matches, `wii_hits=4`.
- After (this round, same gate, fresh run): **7/40 refusals** (all `allow_screen_content_tools`
  -- an unrelated, pre-existing capability gap, `intrabc`/`palette_mode_info` unimplemented),
  **33/40 matches**, `wii_hits=5`. The multi-slot-ROTZOOM refusal string no longer appears at
  all.

## Suite

`cargo test -p ec-av1 --release --lib`: 230 passed, 1 failed (`a_real_aomenc_filter_intra_stream_decodes_pixel_exact`
-- `use_filter_intra never fired decoding this stream`, an aomenc-RD-nondeterminism gate flake,
unrelated to this change; reran standalone and it passed clean). 17 ignored (scratch/manual
tests) as usual.

## Files touched

- `crates/ec-av1/src/mvstack.rs` -- the predictor fallback fix (`find_mv_stack_with_sign_bias`).
- `crates/ec-av1/src/stream.rs` -- the multi-slot-ROTZOOM refusal removed, comment rewritten to
  record the r4->r5->r6 chain for the next reader.

## Next round

`allow_screen_content_tools` is now the widest live refusal in this gate (7/40, was already
present, unrelated to global motion) -- `intrabc`/`palette_mode_info` for inter frames, per the
refusal string's own note. AFFINE-on-single-ref-frame stays refused, unproven, not attempted
this round (charter scoped it out: "AFFINE stays refused unless you prove it separately").

Claude-Session: https://claude.ai/code/session_01T6cfkyThENXszWWQqYpuC4
