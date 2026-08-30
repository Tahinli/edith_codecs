VERDICT: OPEN -- merge landed and verified green; inter-frame superres (spec 7.11.3.3) is fully
derived from libaom source but NOT implemented this round (turn budget spent on the merge +
algorithm derivation, none left to safely wire + verify the decode-side plumbing).

## What was done

1. `git merge main` (a8724cb) into `lane-superres` -- clean, no conflicts (56 files, tile rows,
   delta_q/delta_lf, palette-Y, `bit_depth != 8` refusal, `gate_coverage.rs`/`refusal_inventory.rs`
   landed on main since this branch's `92d8beb`). Committed as `f54f2c9`.
2. `cargo check -p ec-av1`: clean.
3. `EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1` (600s timeout): **256 passed, 0 failed, 17
   ignored** -- no regression from the merge, including
   `a_real_aomenc_superres_key_frame_sequence_decodes_pixel_exact` (key-frame superres, still 3/3
   pixel-exact) and `a_real_aomenc_stream_with_superres_refuses_by_name` (the inter-frame refusal
   this round targets, still firing correctly).

## The algorithm (derived from `~/.cache/aom-oracle`, not yet ported)

Read `av1/common/scale.c`/`scale.h`, `av1/decoder/decodeframe.c`'s `dec_calc_subpel_params`, and
`av1/common/convolve.c`'s `av1_convolve_2d_scale_c` end to end. Key finding that shrinks the real
scope: **AV1 superres only ever scales width** -- `frame_height` is coded directly and never
differs between a stored (upscaled) reference and the frame reading it, so the reference's
`y_scale_fp` is always `REF_NO_SCALE` (16384). Algebraically (`av1_scaled_y` with
`y_scale_fp==16384` reduces to `val << SCALE_EXTRA_BITS` exactly, a pure precision-widening
no-op), this means the **vertical pass is untouched** -- this decoder's existing `y_q4`
computation (`mv_to_q4`) and 16-phase filter selection in `mc.rs` stay bit-exact as-is. Only the
**horizontal pass** needs a per-column scaled step instead of a fixed stride-1 walk.

Further, the non-compound rounding chain in `av1_convolve_2d_scale_c` (`round_0=3`, `round_1=11`,
the `bd+FILTER_BITS-1` / `offset_bits` bias constants added before each shift) was hand-proven to
cancel out exactly to this crate's existing `round2(sum, INTER_ROUND_0)` /
`round2(sum, INTER_ROUND_1).clamp(0,255)` (the bias terms are always exact multiples of the
shift divisor because every filter's 16 taps sum to 128, `FILTER_BITS` unity gain) -- so the
**scaled path needs no new rounding math**, only a new per-column integer-position/filter-phase
walk for the horizontal pass:

```
x_scale_fp   = ((ref_luma_width << 14) + cur_luma_frame_width / 2) / cur_luma_frame_width   // REF_SCALE_SHIFT=14
x_step_qn    = round_pow2(x_scale_fp, 4)                                                     // Q10 (SCALE_SUBPEL_BITS=10)
off          = (x_scale_fp - 16384) * 8
pos_x_q10    = round_pow2_signed_64(x_pos_q4 * x_scale_fp + off, 8) + 32                      // x_pos_q4 == mv_to_q4(px, mv.1, luma), same value already computed at every call site
// per output column c:
x_qn         = pos_x_q10 + c * x_step_qn
int_pel      = x_qn >> 10
filter_idx   = (x_qn & 1023) >> 6        // indexes the SAME 16-entry SUBPEL_FILTERS* tables already in mc.rs
```

`x_scale_fp` is derived from **luma** widths only (the stored reference's own width vs the
current frame's `frame_width`) and applies unchanged to chroma calls (their `x_pos_q4` is
already in the chroma plane's own pixel units, so the ratio is correct without adjustment).
`av1_scale_mv`/`clamp_mv_to_umv_border_sb`/the border-extension memcpy path
(`update_extend_mc_border_params`/`build_mc_border`) are NOT needed: this decoder already
clamps every tap to `true_width`/`true_height` per-sample in `mc::sample`, the same corner-cut
the existing unscaled path already relies on (no block-position pre-clamp exists there either).

## Why not landed this round

`decode_inter_block` (`decode.rs:7332`) has **19 call sites** inside
`decode_inter_frame_tile_with_cdfs`, one per partition-shape arm, all needing a new
`frame_width: usize` argument threaded through (the enclosing function already has `frame_width:
u32` in scope, so this is mechanical but wide). The actual scaled-MC call swap only needs to
happen at the single-reference non-warp/non-OBMC/non-interintra branch (`decode.rs:8671-8711`,
the common case this lane's gate would exercise); every OTHER combination (compound, warp, OBMC,
interintra reaching a scaled reference) needs a narrow named refusal added at its own call site
(`decode.rs:~7994` for compound, `~8670` for warp/OBMC/interintra) comparing that block's
`py_ref.width`/`py0.width`/`py1.width` against `frame_width` -- silent wrongness otherwise, since
lifting the blanket frame-level `use_superres && frame_type != Key` refusal in `stream.rs:217`
makes every one of those paths reachable with no scaling support.

Given the turn-budget checkpoint hit mid-way through mapping these 19 call sites (paren-balanced
script output captured, see below), landing a partial edit here risked the class
`builders-lose-work-at-the-cap` (uncommitted mid-edit at cap) rather than a clean stop. No code
was touched this round beyond the merge -- `git status` is clean on top of `f54f2c9`.

## Next round's exact TODO (no further libaom reading needed)

1. Add `mc::predict_scaled` to `mc.rs`: same signature shape as `predict_with_filters` plus
   `x_scale_fp: i64` in place of one `x_q4`, keeping `y_q4` as-is; implement per the pseudocode
   above (horizontal pass only differs; vertical pass copy-pasted verbatim from
   `predict_with_filters`). Add `mc::scale_factor(other: usize, this: usize) -> i64` and a
   `REF_NO_SCALE: i64 = 1 << 14` constant.
2. Add `frame_width: usize` to `decode_inter_block`'s parameter list (after
   `allow_screen_content_tools` is the existing convention for the newest param). Thread
   `frame_width as usize` at each of the 19 call sites -- **all but one already end their
   argument list with `allow_screen_content_tools,`** (paren-balanced scan this round, call sites
   at `decode.rs:10633,10708,10887,10939,10992,11055,11103,11156,11204,11258,11310,11362,11415,
   11467,11519,11573,11621,11673`); the 19th (`decode.rs:12528`) ends its list with `false,`
   instead (likely a different helper's inline call, or `decode_inter_block8` -- re-check before
   assuming the same insertion point). A single scripted `sed`/python insert after the last arg
   line at each of the 18 confirmed sites plus one hand-check at 12528 is the mechanical way in.
3. At `decode.rs:8670` (right before `let mut pred_y = ...`, after `warp_params`/`obmc_selected`/
   `interintra_mode` are all resolved and every symbol for this block has been read): if
   `warp_params.is_some() || obmc_selected || interintra_mode.is_some()`, refuse by name when
   `mc::scale_factor(py_ref.width, frame_width) != mc::REF_NO_SCALE`; else branch unscaled
   (existing `predict_with_filters`, unchanged call) vs scaled (`mc::predict_scaled`) per luma and
   both chroma planes.
4. At `decode.rs:7994` (right after `resolve_interp_filter` in the compound branch, before
   `predict_compound_intermediate`): refuse by name when either `py0.width` or `py1.width` scaled
   vs `frame_width`.
5. Gate FIRST (charter's own order): a bypass test mirroring
   `a_real_aomenc_stream_with_two_tile_rows_decodes_pixel_exact`'s pattern (`stream.rs:6647`) --
   decode frame 0 (key, `use_superres`) through `decode_key_frame_tile_with_cdfs` directly, feed
   its picture as `reference` into `decode_inter_frame_tile_with_cdfs` directly for frame 1+
   (`--superres-denominator=12 --kf-max-dist=1000`, disabling warp/obmc/compound/interintra the
   same way the key-frame gate disabled intra tools), bypassing `decode_stream`'s frame-level
   refusal entirely so the new code path is what gets measured, per the charter's own instruction.
6. Only once that bypass gate is pixel-exact: lift the `stream.rs:217` refusal (scope it to the
   combos still refused per steps 3/4 rather than deleting outright) and update
   `refusal_inventory.rs` in the same commit, plus a `decode_stream`-level gate re-proving it
   the same way `a_real_aomenc_stream_with_two_tile_rows_decodes_through_decode_stream` re-proves
   tile rows.

## Refusal strings

None added, renamed, or removed this round -- `refusal_inventory.rs` untouched, no capability
claim made.

## Merge

Done, committed (`f54f2c9`), suite green (256/0/17). This satisfies the charter's "merge main and
resolve first" step.

## Disposition

- deferred: inter-frame superres itself (spec 7.11.3.3 scaled MC) -- algorithm fully derived
  (this report), wiring is mechanical but wide (19 call sites); next lane round, no further
  libaom source reading needed, start at step 1 above.
- fix-now: none this round -- no defect found, no defect fixed.
