VERDICT: CLOSED -- the superres gate is pixel-exact; charter hypothesis 1 was the root cause.

## What was tested

Charter hypothesis 1: "libaom clips the deblock loop to the coded frame's width, our
`true_width`/`true_height` (mi-aligned, `mi_cols*4`/`mi_rows*4`) includes a padding
margin libaom never filters." Confirmed on the first try -- did not need hypothesis 2
(margin vs replicated-border upscaler feed) or the gdb dispatch hunt.

## Fix

`crates/ec-av1/src/decode.rs`: `deblock_plane` (and its caller `apply_deblock`) now take
`frame_width`/`frame_height` and bound their `x0`/`y0` loops by
`plane.true_width.min(cw)` / `plane.true_height.min(ch)` where `cw`/`ch` are the coded
frame's own dimensions (`frame_height.div_ceil(2)` for chroma, matching the 4:2:0
subsampling this decoder assumes), instead of always looping the full mi-aligned
`true_width`/`true_height`. Both call sites (`decode_key_frame_tile_with_cdfs`,
`decode_inter_frame_tile_with_cdfs`) updated to pass `frame_width as usize, frame_height
as usize`.

## Ground truth, not just gate-green

Read libaom's `set_lpf_parameters` (`av1/common/av1_loopfilter.c:224`) while removing the
throwaway trace lines there (see Housekeeping below) and found the actual normative
check directly:

```c
const uint32_t width = plane_ptr->dst.width;
const uint32_t height = plane_ptr->dst.height;
if ((width <= x) || (height <= y)) {
  // just return the smallest transform unit size
  return TX_4X4;
}
```

`plane_ptr->dst.width`/`height` is the destination plane's coded width/height (the same
quantity our `frame_width`/`frame_height` already carries), not the mi-grid-padded
extent. This is the third dispatch path r6 was hunting for with `EC_LF_TRACE`/
`EC_LF_TRACE2` at `set_lpf_parameters`/`set_one_param_for_line_luma` -- both traces r6
added never fired because they were placed *after* this early return already skipped
the block; the trace at line 298 (inside `set_lpf_parameters`, guarded on
`edge_dir==HORZ_EDGE && x==44`) sat downstream of this `if`, so it correctly never
printed for x=44 once x exceeded `plane_ptr->dst.width` -- r6's "neither candidate
fires" was itself the evidence for hypothesis 1, not a dead end (class
`equal-range-means-unread`, per the charter).

## Verification

- `cargo check -p ec-av1`: clean.
- `EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib stream::tests::a_real_aomenc_superres_key_frame_sequence_decodes_pixel_exact -- --nocapture`:
  `3 frames pixel-exact, superres_hits=3` -- **PASS** (was the lane's one failing gate
  since r5).
- `EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1` (full crate, 600s timeout): **238
  passed, 0 failed, 17 ignored** -- no regression from the clip (the mi-alignment
  margin only ever held decode scratch space beyond the coded picture; nothing else
  reads or filters it).

Commit: `f64f5cd` on `lane-superres` (this worktree).

## Housekeeping

r6's throwaway `EC_LF_TRACE`/`EC_LF_TRACE2` `fprintf` diagnostics in
`~/.cache/aom-oracle/src/av1/common/av1_loopfilter.c` (lines 298-301, 714-717) removed --
their placement (downstream of the early-return this round found) is itself part of the
evidence above, so they are recorded in this report rather than folded into
`scripts/instrument-aom-oracle.sh` as a permanent rung; the oracle source tree is back to
a pristine, uncommitted state. Not rebuilt (`ninja`) this round since nothing further
needed the oracle binary -- `scripts/build-aom-oracle.sh` will produce a clean build from
this source on next use, no stale trace code left behind.

## Refusal strings

None added, renamed, or removed this round.

## Merge

Not attempted -- turn budget went to landing and verifying the fix + housekeeping. Left
for the next lane; charter's "consider `git merge main` here" is conditional on budget
and main has moved substantially (per r6's merge note) since `92d8beb`.

## Disposition

- deferred: `git merge main` (charter's note: multi-tile decode, CDEF index, chroma
  modes, `bit_depth != 8` refusal, three reworded partition refusals, three guard tests
  landed there since 92d8beb) -- next lane in this worktree, or whoever picks this branch
  up, should merge and re-run the full `ec-av1` suite before anything further.
- deferred: Stage 4, inter-frame superres (spec 7.11.3.3, scaled-reference MC,
  `av1_setup_scale_factors_for_frame` / `av1_convolve_2d_scale`) -- unstarted, whole
  stage, per charter's "Then" section.
