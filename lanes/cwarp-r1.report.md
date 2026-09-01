# lane-cwarp r1 -- per-reference GLOBAL warp on compound blocks

VERDICT: DONE for the 8-bit path (new hard gate green, 37 real compound-warp
blocks). 10-bit arm is written but `#[ignore]`d, blocked by a PRE-EXISTING
10-bit *compound* MC defect this lane isolated but does not own.

## Defect (charter premise re-measured, held)
`decode_inter_block`'s COMPOUND_REFERENCE branch predicted BOTH taps
translationally through `mc::predict_compound_intermediate`. libaom runs
`av1_init_warp_params` inside its per-reference loop
(`reconinter.c build_inter_predictors_8x8_and_bigger`), so each reference of a
`GLOBAL_GLOBALMV` block whose own gm model is > TRANSLATION is filtered through
`av1_warp_plane` into the compound intermediate. No refusal named this: the
pixels were silently wrong. Confirmed live -- the new gate hits 37 such blocks
in 6 real aomenc streams.

`WARPED_CAUSAL` on a compound block: confirmed impossible, left alone.
libaom `decodemv.c read_motion_mode` -> `motion_mode_allowed` returns
SIMPLE_TRANSLATION for `has_second_ref(mbmi)`; spec 5.11.27 the same. So only
the GLOBAL arm of `allow_warp` is live on the compound path.

## Changed
- `crates/ec-av1/src/warp.rs`
  - `warp_affine` split into a shared `warp_inner` + a store closure; the old
    hardcoded `const BD: i32 = 8` is now `crate::decode::bit_depth()` (the same
    thread-local `clip_pixel`/`sample_max` already use, so no call site moved).
    `REDUCE_BITS_HORIZ`/`COMPOUND_ROUND1_BITS` named from libaom's
    `conv_params->round_0`/`COMPOUND_ROUND1_BITS`.
    NOTE for lane-hbdinter: this is the whole of the warp bit-depth change --
    one `bd` local and two offset expressions, no signature change.
  - new `warp_affine_compound`: `av1_highbd_warp_affine_c`'s
    `conv_params->is_compound && !do_average` branch -- vertical pass rounds by
    `round_1 == 7` and the output stays in the compound intermediate domain.
    libaom's CONV_BUF carries a constant bias that its blend subtracts
    (`(1 << (offset_bits - round_1)) + (1 << (offset_bits - round_1 - 1))`);
    this crate's intermediates are unbiased, so exactly that constant --
    `(1 << (bd+4)) + (1 << (bd+3))` -- is removed here, making the output
    interchangeable with `mc::predict_compound_intermediate`'s for
    `combine_compound`/`blend_masked_compound`. Both offset terms survive their
    roundings exactly (each exceeds its shift), so bias removal is bit-exact,
    not an approximation.
- `crates/ec-av1/src/decode.rs`
  - compound branch: `is_global_mv0`/`is_global_mv1` hoisted out of the MiGrid
    stamp (and their size bound corrected from `bw4.min(bh4) >= 2`, which used
    the frame-CLIPPED write extent, to `side >= 8`, libaom's unclipped `bsize`
    -- differs only for a block whose visible extent is 4px);
    `compound_warp(ref, eligible)` builds each ref's `global_warp_params`
    (skipped on `force_integer_mv` or `gm.invalid`, mirroring
    `av1_init_warp_params` + `allow_warp`), and each of the six
    `predict_compound_intermediate` outputs is replaced by
    `warp_affine_compound` when that ref warps. Chroma is gated on
    `chroma_side >= 8` -- `av1_init_warp_params` returns early on
    `block_width < 8`, and that is the PLANE's block, so an 8x8 luma block's
    4x4 chroma stays translational.
  - new `COMPOUND_WARP_HITS` counter + `compound_warp_hits()`.
- `crates/ec-av1/src/mc.rs` -- `diffwtd_mask`'s round is
  `2*FILTER_BITS - round_0 - round_1 + (bd - 8)` (libaom `diffwtd_mask_d16`,
  reconinter.c:306); the `(bd - 8)` term was missing (this function's own doc
  comment already named it), so every 10-bit DIFFWTD mask was 4x too coarse.
  Found while bisecting the 10-bit failure. 8-bit unaffected (`bd-8 == 0`).
  UNGATED: the only 10-bit compound gate is the ignored one below.
- `crates/ec-av1/src/stream.rs` -- `run_compound_global_warp_gate` + the two
  gates below. No refusal string existed for this defect, so none is lifted;
  `refusal_inventory.rs`/`gate_coverage.rs` needed no change (both green).

## Gate
`a_real_compound_global_warp_stream_decodes_pixel_exact` (8-bit): rotating
mandelbrot (`rotate=a=RATE*n` over a 2x source, scaled to 64x64), 24 frames,
`--enable-global-motion=1 --enable-dist-wtd-comp=1 --lag-in-frames=25
--auto-alt-ref=1 --enable-warped-motion=1 --enable-masked-comp=1`, 3 cq x 2
rotation rates. Hard-asserts `compound_warp_hits() > 0` and `matched > 0`; a
decode error or a pixel mismatch is a panic, never a skip.

EVIDENCE: cargo test -p ec-av1 --release --lib -- a_real_compound_global_warp --nocapture (EC_AV1_REQUIRE_AOMENC=1) | 6 real aomenc encodes, every display frame's Y/U/V vs ffmpeg | `6/6 pixel-exact, 0 named refusals, compound_warp_hits=37`

EVIDENCE: same command with `--enable-global-motion=0 --enable-warped-motion=0` and, separately, `--auto-alt-ref=0 --lag-in-frames=0 --enable-dist-wtd-comp=0` (10-bit arm) | 2 ablation runs | warp-free 10-bit compound FAILS at frame 1 luma cq 32; 10-bit single-ref inter is 6/6 pixel-exact -> the residual is 10-bit COMPOUND, not warp

EVIDENCE: cargo test -p ec-av1 --release --lib (EC_AV1_REQUIRE_AOMENC=1) | whole ec-av1 lib suite | **271 passed, 0 failed, 23 ignored**, 312s

Hidden frames: the gate encodes with `--auto-alt-ref=1 --lag-in-frames=25`, so
hidden alt-refs exist and every shown frame that references them is compared;
a wrong hidden-frame reconstruction propagates into those. There is no
decode-order oracle compare here (the repo's `EC_AV1_DECODE_ORDER_DUMP` rung is
8-bit-only and manual) -- same standard as the other compound gates.

## Residue
- deferred(the 10-bit compound MC defect): `a_real_compound_global_warp_10bit_stream_decodes_pixel_exact`
  is `#[ignore]`d with the ablation in its doc comment. Unblocked by whoever
  owns 10-bit compound MC; un-ignore it then -- the warp path under it is
  already bit-depth generic.
- flagged (same defect shape, NOT fixed): `decode_inter_block8`'s compound
  branch (decode.rs ~10969-11072 on this branch) has the identical
  translational-only compound prediction. For an 8x8 leaf the luma block is
  exactly 8x8, so libaom WOULD warp luma there (chroma 4x4 would not). Not
  wired this round: the 8x8 leaf's own gates are the subject of lane-gmaffine
  and a change here would land untested.
- accepted: `mc::diffwtd_mask`'s `(bd - 8)` fix is source-verified against
  libaom but has no green 10-bit gate behind it yet (see above).
