# lane-golomb r8 -- two true-size defects fixed, straddle arms STILL not addable

Tip: `97de75e` on `lane-golomb` (merge `3a08902` of main `85887c7`, then the fix).

## (1) Suite r7 -- no result line, superseded
`$HOME/.cache/golomb-suite-r7.log` was still `active` at **226/404** tests with **no**
`test result:` line and **zero** `FAILED`/`panicked` lines. It was measuring the PRE-merge
tree (main has since advanced to 85887c7), so r8 stopped the unit to take the target dir.
Owed artifact carried forward to r8's own suite.

## (2) Merge of main 85887c7
`git merge --no-commit main` was a CLEAN auto-merge (decode.rs + stream.rs auto-merged, no
conflict, no hand resolution). Byte-compared against main afterwards:
`mvstack.rs`, `cdf.rs`, `cdf_state.rs`, `refusal_inventory.rs` are all **IDENTICAL to main**
(`git diff --quiet main -- <file>`). Only decode.rs and stream.rs carry lane content.
Merge commit `3a08902`.

EVIDENCE: $HOME/.cache/golomb-gate-r8h.log | edge32 gate on the merged tree, all 8 cq arms x
{192x80, 80x192} x {8,10}-bit + the 5-frame inter arm | `test result: ok`, 38 pixel-exact
attempts, 64-level edge bits [horz_or_vert=7 split=197] right-VERT=7, 2 named refusals --
byte-identical totals to r7's green run.

## (3a) FIXED: a same-size reference at a height that is not a multiple of 8
`crates/ec-av1/src/decode.rs`, `decode_inter_frame_tile_with_cdfs`: the guard compared
`reference.height` against `true_height = (mi_rows * 4)` (mi-rounded). A decoded `Picture`
is cropped to the header's `fh`, so at height 68 (`mi_rows = 2*((68+7)>>3) = 18` -> 72) it
compared **68 against 72** and refused every same-size reference.
libaom compares references on the CROPPED dimensions --
`av1_setup_scale_factors_for_frame(&sf, ref->y_crop_width, ref->y_crop_height, cm->width,
cm->height)` -- and calls a reference *scaled* only when those differ (`av1_is_scaled`).
Height must still match (AV1 superres never scales height). Class `av1-truesize-lane`.

EVIDENCE: $HOME/.cache/golomb-gate-r8{b,f}.log | `arms.push((192, 68, 5, false))`, cq sweep |
BEFORE: 8/8 attempts refuse "a reference picture whose height does not match this frame's own
true size"; AFTER: 0/8 do.

## (3b) NOT the cause of the 68x192 column defect -- charter hypothesis REFUTED at the source
The charter proposed our reference plane replicates from the mi-rounded column 71 instead of
the true column 67. Read `aom_scale/generic/yv12extend.c`: `aom_yv12_extend_frame_borders_c`
calls `extend_plane(..., ybf->crop_widths[is_uv], ybf->crop_heights[is_uv], ...,
right = plane_border + widths - crop_widths)` and `extend_plane` memsets from
`src + width - 1` -- i.e. libaom OVERWRITES the decoded columns 68..71 with column 67.
Our `mc::sample` clamps at `true_width - 1` of the CROPPED reference `Picture` (68) -->
already identical. No fix needed; the 141-px defect is elsewhere.

## (3c) FIXED (source-cited, UNEXERCISED): loop restoration ran over the mi-rounded plane
`decode.rs::apply_loop_restoration` passed each plane's `true_width`/`true_height`. LR is the
one post-decode filter that does not walk the mi grid: libaom sizes every plane with
`av1_get_upsampled_plane_size` (`plane_w = ROUND_POWER_OF_TWO(cm->superres_upscaled_width,
ss_x)`, `av1/common/restoration.c:47`), **asserts** it equals the buffer's crop width
(`restoration.c:1106`) and `av1_extend_frame(..., plane_w, plane_h, ...)` replicates the
border from that cropped edge before any unit is filtered (`restoration.c:1109`). Deblock and
CDEF DO walk the mi grid, which is why this survived until a non-multiple-of-8 size.
Now takes `(frame_width, frame_height)`, chroma `div_ceil(2)`.
DISCLOSURE: **no gate exercises this** -- every straddle recipe in `edge32_gate` runs
`--enable-restoration=0`, and the 68x192 defect was byte-identical before and after
($HOME/.cache/golomb-gate-r8{c,d}.log, both `141 pixels differ, first at row 0 col 64`).
It is a source-cited correctness alignment, verified only by the suite staying green.

## (4) The straddle arms are STILL not addable -- three separate blockers, each measured
Charter asked for `(192,68,5)` and `(68,192,5)` arms at 8+10 bit with a straddling-TU counter.
DEVIATION: no arm was added, because every cq level of both is vacuous, panicking or red.
The measurements are now recorded verbatim in the `edge32_gate` comment block:
* `192x68` cq35 8-bit: no longer the height refusal -- refuses LATER by another lane's name,
  "an inter SB-level AB partition (HORZ_A/HORZ_B/VERT_A/VERT_B)". Vacuous
  (class `counter-from-refused-stream`).
* `192x68` cq40 8-bit: **PANICS** in `decode_inter_block8` -- `from_switchable_symbol` handed a
  4th symbol, `mc.rs:203`. That is lane-sub8's own open below-8x8 desync (already named in
  `decode.rs`'s scaled-reference-8x8 refusal comment). A panic cannot be tolerated as a named
  refusal, so the arm cannot even be added with an Err-tolerating recipe.
* `192x68` cq59 8-bit: RED, `frame 1 plane Y: 217 pixels differ, first at row 60 col 170
  (ours 172 vs ffmpeg 173)`.
* `192x68` cq35 10-bit: RED, `frame 1 plane Y: 305 pixels differ, first at row 56 col 48
  (ours 530 vs ffmpeg 531)`.
* `68x192` cq35 8-bit: RED, `frame 1 plane Y: 141 pixels differ, first at row 0 col 64
  (ours 167 vs ffmpeg 166)` -- unchanged from r7 by either fix this round.

NEW FACT for the successor: all four red cases are **+-1** bands, on **frame 1** (the first
INTER frame -- frame 0 is bit-exact in every one), and they sit in or immediately beside the
4-px straddling band (rows 56..63 next to straddling rows 64..67; columns 64..67 are the
straddling columns themselves). That is one straddling-band INTER reconstruction defect with
two orientations, not four defects -- and it is not the reference border (3b), not LR (3c) and
not intra (frame 0 exact).

The gate now prints each attempt under `EC_GATE_VERBOSE=1` (`stream.rs`), because a desync
otherwise surfaces as a bare panic in `mc.rs` with no arm named.

## Test totals
`cargo test -p ec-av1 --lib` -> `$HOME/.cache/golomb-suite-r8.log` (unit `golomb-suite-r8`):
`test result: ok. 379 passed; 0 failed; 34 ignored; 0 measured; 0 filtered out; finished in
917.74s` (413 test lines emitted). GREEN.

EVIDENCE: $HOME/.cache/golomb-suite-r8.log | `cargo test -p ec-av1 --lib` on the merged tree at
97de75e, unit golomb-suite-r8, MemoryMax=10G | 379 passed / 0 failed / 34 ignored.

## Residue disposition
* 68x192 / 192x68 straddling-band +-1 INTER defect -- **fix-now, next round**; it is this
  lane's whole remaining job and the successor has the four measurements above.
* `192x68` cq40 below-8x8 panic -- **deferred(lane-sub8's open desync)**; not this lane's code.
* `192x68` cq35 inter SB-level AB partition refusal -- **deferred(the AB-partition lane)**.
* LR cropped-plane fix ungated -- **deferred(a straddle gate arm with
  `--enable-restoration=1`)**; the base recipe disables restoration for every arm.
