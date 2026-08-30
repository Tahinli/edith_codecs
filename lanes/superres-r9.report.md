# lane-superres round 9

## Root cause

`decode_inter_frame_tile_with_cdfs`'s `LAST_FRAME_WIDE_MARGIN` write site
(`crates/ec-av1/src/decode.rs`, was ~line 12744) stashed the real-decoded
margin for superres's right-edge replicate using the superblock-padded
`width`/`height` (multiple of `BLOCK`), instead of the mi-aligned
`true_width`/`true_height` (`mi_cols * 4` / `mi_rows * 4`) that the
key-frame branch (`decode_key_frame_tile_with_cdfs`) already used
correctly. Columns `[true_width, width)` were never actually coded (the
last partial superblock stops at `true_width`) -- for this fixture's inter
frame, luma's `true_width` happens to equal its padded `width` (64 == 64,
no margin needed at all for Y), but chroma's `true_width/2` is 1 column
short of `width/2`, so the stashed chroma margin buffer included one
column of stale/uninitialized `PlaneBuf` content past the real decoded
extent. `upscale_row`'s right-edge replicate then read that stale column
one position early, producing the observed "wrong sample one column
early, then an un-clamped ramp value" shape -- exactly the
shorten-vs-replicate class this repo has hit before (`av1-truesize`,
`reference-layout-not-spec`).

This matches the round's own hypothesis #3 (key frame's gate passes only
because its dimensions make chroma-width parity forgiving) but the actual
mechanism is #2 (a wrong-width clamp/crop bound in the margin fetch), not
a step/x_step_qn derivation bug -- `upscale_plane`'s per-plane
`chroma_in_w`/`chroma_out_w` (`superres.rs`) were already correct
(`div_ceil(2)` of luma width, matching libaom's `(w + ss_x) >> ss_x`), and
`upscale_convolve_step`/`x0` are already derived from the plane's own
in/out widths, not a shifted luma step. Confirmed by inspection + the
fix: changing only the margin crop bound (not the step/width math) makes
the gate pass.

## Fix

`crates/ec-av1/src/decode.rs`, inter-frame margin write site: use
`true_width`/`true_height` (already in scope, used by `y`/`u`/`v`
`PlaneBuf` construction earlier in the same function) as both the
Some/None condition and the crop bound, mirroring the key-frame branch
line for line.

## Per-plane widths (this fixture, 64x64 upscaled, superres-denominator=12)

Not separately re-derived by instrumentation this round -- the fix target
was confirmed directly by code inspection (the inter branch's margin crop
used the wrong width variable, visible by diffing it against the correct
key-frame branch a few thousand lines earlier in the same file) and by
the gate flipping from failing-at-U-plane to passing. `EC_AV1_TRACE`/
`EC_AV1_MARGIN_DUMP` instrumentation was not needed once the two branches
were compared side by side.

## Gate result

```
EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib a_real_aomenc_key_and_inter_superres -- --nocapture
```
```
a_real_aomenc_key_and_inter_superres_sequence_decodes_pixel_exact: 3 frames pixel-exact, superres_hits=3, predict_scaled_hits=36
test stream::tests::a_real_aomenc_key_and_inter_superres_sequence_decodes_pixel_exact ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 283 filtered out; finished in 0.18s
```

## Full suite

```
EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib
```
`265 passed; 0 failed; 19 ignored; 0 measured; 0 filtered out; finished in 168.30s`

## Workspace check

`cargo check --workspace --all-targets` -- clean (only pre-existing
`missing documentation` / `unused_parens` warnings, no errors).

## HEAD

Committed at this lane's tip after this report (see `git log -1`).
