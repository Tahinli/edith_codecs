VERDICT: WIP -- stage-3 gate closer but not green; two real fixes landed, one residual defect open

## What changed (commit 85c070b, branch lane-superres)
- `crates/ec-av1/src/stream.rs`: gate
  `a_real_aomenc_superres_key_frame_sequence_decodes_pixel_exact` got the
  charter's restrictive aomenc flag set, which cleared the pre-existing
  "AV1 tile: partition type this encoder never writes" capability gap
  (confirmed pre-existing, not a superres defect, per charter).
- Same gate: added `--superres-kf-denominator=12`. Root cause found by
  tracing libaom `av1/encoder/superres_scale.c` `calculate_next_superres_scale`:
  under `AOM_SUPERRES_FIXED`, key frames read
  `superres_cfg->superres_kf_scale_denominator`, non-key frames read
  `superres_scale_denominator` -- two separate CLI flags
  (`--superres-denominator` only sets the latter). This gate's fixture uses
  `--kf-max-dist=0` (every frame a key frame per its own doc comment), so
  without the kf flag `superres_kf_scale_denominator` stayed at its default
  8 (no scale) and `header.use_superres` decoded `false` -- confirmed with a
  temporary debug print, then reverted.
- `crates/ec-av1/src/decode.rs`: real decoder bug, both
  `decode_key_frame_tile_with_cdfs` (line ~4993) and
  `decode_inter_frame_tile_with_cdfs` (line ~10504) cropped chroma planes
  to `fw / 2, fh / 2` (floor) when `frame_width` differs from the
  mi-aligned decode buffer width. AV1's 4:2:0 chroma-plane width is
  `ROUND_POWER_OF_TWO(fw, 1)` (div_ceil), not floor -- confirmed against
  libaom `resize.c:1124`. Odd downscaled widths (43, from this superres
  fixture) exposed it as a `debug_assert_eq!` panic in `superres.rs:135`
  (`upscale_plane` got a 21-wide chroma row, expected 22). Fixed both call
  sites -- grepped, these are the only two constructors of the cropped
  `Picture`, class swept.

## What's still broken
Gate still fails: 5 of 4096 luma pixels off by exactly 1, all near the
right edge (x in 59..62 of a 64-wide frame). Ruled out:
- The upscale kernel itself (`upscale_row`/`upscale_plane` in
  `superres.rs`) is pinned bit-exact against real libaom
  `av1_convolve_horiz_rs_c` via the C harness for symmetric scale cases
  (in8->out12, in8->out16) -- r1's work, unchanged this round.
- Not a bounds bug: traced the padding math (`pad=10`, `base=6`,
  `UPSCALE_NORMATIVE_TAPS=8`) by hand, all array indices stay in range.

Leading theory, NOT confirmed: `decode.rs` crops the plane to exactly
`frame_width` (43) before `superres::upscale_picture` ever sees it, so
`upscale_row`'s right-edge padding is a synthetic replicate of column 42.
libaom runs `av1_superres_upscale` on the reconstructed buffer's own
border-extended margin, which may still hold a real decoded pixel at
mi-alignment column 43 rather than a replicate -- untested this round
(would need threading the pre-crop plane + real width into
`upscale_picture` instead of cropping to `fw` first). Diff pattern (only
5 isolated pixels, not a monotonic edge band) is also consistent with a
subtler rounding-phase edge case; not conclusively attributed either way.

## Order of work not reached
- Full scoped suite (`EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib
  -j4`) not run this round -- the target gate itself was still red, ran
  `cargo check -p ec-av1 --lib --tests` only (clean) to confirm no
  compile breakage from the two fixes before committing.
- Stage 4 (inter-frame superres, spec 7.11.3.3) not started -- blocked on
  stage 3 closing first per the charter's own ordering.

## Commands
- `cargo check -p ec-av1 --lib --tests` -- clean, 85 pre-existing doc
  warnings only.
- `EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib
  a_real_aomenc_superres_key_frame_sequence -j4 -- --nocapture` --
  FAILED: `frame 0 luma vs ffmpeg`, 5/4096 pixels off by 1 (measured via
  temporary debug instrumentation, reverted before commit).

## Handoff for next round
1. Try threading real pre-crop border pixels into `upscale_picture`
   instead of edge-replicating from the already-cropped row -- needs a
   small API change (`upscale_picture` takes the plane's real allocated
   width + a slice wide enough to cover `UPSCALE_NORMATIVE_TAPS/2+1`
   extra real columns) plus a `decode.rs` change to stop discarding that
   margin before calling it.
2. If that doesn't close it, dump per-pixel `x_qn`/`int_pel`/`filter_idx`
   at the 5 failing output columns and hand-trace against a libaom debug
   build of the same fixture -- the diffs are too sparse for a formula
   bug, more likely a specific phase/rounding edge case.
