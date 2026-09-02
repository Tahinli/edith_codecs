# lane-golomb r8 HANDOFF -- two true-size fixes in, suite GREEN, straddle arms still blocked

Tip: `lane-golomb` (report `lanes/golomb-r8.report.md`, fix `97de75e`, merge of main
`85887c7` at `3a08902`). Suite: `379 passed; 0 failed; 34 ignored`
(`$HOME/.cache/golomb-suite-r8.log`). edge32 gate green, 38 pixel-exact attempts, 2 refusals.

## Landed
1. Reference-height refusal compared the mi-rounded `true_height` with the cropped
   `reference.height`; now compares the header's `frame_height` (libaom compares references on
   `y_crop_width/height`). Every 192x68 attempt used to refuse; none do now.
2. `apply_loop_restoration` now takes the cropped `(frame_width, frame_height)` instead of
   plane `true_width/true_height` (libaom `av1_get_upsampled_plane_size` +
   `assert(plane_w == crop_widths)` + `av1_extend_frame`). UNGATED -- every straddle recipe
   runs `--enable-restoration=0`.

## The ONE open defect this lane still owns
A **+-1 straddling-band INTER reconstruction defect**, both orientations, all on **frame 1**
(frame 0 is bit-exact in every case, so it is not intra and not a per-frame filter constant):
* `68x192` cq35 8-bit: `frame 1 plane Y: 141 px differ, first at row 0 col 64 (167 vs 166)`
  -- cols 64..67 are the straddling COLUMNS of a 68-wide frame (mi-rounded 72).
* `192x68` cq59 8-bit: `frame 1 plane Y: 217 px, first at row 60 col 170 (172 vs 173)`.
* `192x68` cq35 10-bit: `frame 1 plane Y: 305 px, first at row 56 col 48 (530 vs 531)`.
Reproduce by re-adding `arms.push((68, 192, 5, false))` / `arms.push((192, 68, 5, false))` in
`edge32_gate` (`crates/ec-av1/src/stream.rs`, the comment block just above `let cqs`) and
running with `EC_GATE_VERBOSE=1` (new this round -- names each attempt before decoding it,
because a desync otherwise panics in `mc.rs` with no arm in the message).

RULED OUT this round, do not re-attempt:
* Reference border padding. `aom_yv12_extend_frame_borders_c` calls `extend_plane` with
  `crop_widths/crop_heights` and memsets from `src + width - 1`, so libaom OVERWRITES decoded
  columns 68..71 with column 67 -- exactly what our `mc::sample` clamp at `true_width-1` of the
  cropped reference already does. The charter's hypothesis (b) is refuted at the source.
* Loop restoration. The gate recipe has `--enable-restoration=0`; the 68x192 diff is
  byte-identical before and after fix 2 (`golomb-gate-r8c.log` vs `r8d.log`).
* Var-tx clipping (`max_w_mi`/`max_h_mi`). libaom's `max_block_wide` is derived from
  `mb_to_right_edge`, i.e. `cm->mi_params.mi_cols` -- the MI-ROUNDED bound, which is what we
  use. Correct as-is.

Next probe to run (r8 ran out of budget before it): dump the 68x192 cq35 stream to disk, then
bisect the frame-1 divergence with `EC_AV1_PREFILT_DUMP` (8-bit vars -- the 16-bit ones
SEGFAULT the oracle aomdec on an 8-bit stream) against the instrumented aomdec. If the
divergence is already present pre-filter it is inter prediction/residual in the straddling
band; if not, it is deblock/CDEF at the x=68 (resp. y=68) mi edge.

## Two blockers that are NOT this lane's code
* `192x68` cq40 8-bit **PANICS** in `decode_inter_block8` (`from_switchable_symbol` handed a
  4th symbol, `mc.rs:203`) = lane-sub8's open below-8x8 desync. A panic cannot be tolerated
  as a named refusal, so no straddle arm can span cq40 until sub8 lands.
* `192x68` cq35 8-bit refuses "an inter SB-level AB partition (HORZ_A/HORZ_B/VERT_A/VERT_B)"
  even though the recipe sets `--enable-ab-partitions=0` -- worth one measurement of its own.
