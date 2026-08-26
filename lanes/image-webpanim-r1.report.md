# lane-image-webpanim r1 — animated WebP

## What landed

`webp::is_animated`, answered from the container alone (the VP8X animation flag
or an `ANIM`/`ANMF` chunk), and `webp::decode_frames`, which walks the `ANMF`
chunks and composites them onto the canvas: frame rectangles, both blending
methods, dispose-to-background applied before the next frame is drawn, and the
duration of each frame in milliseconds. `decode_animation` routes an animated
WebP here; `decode` still refuses one by name.

The still and animated paths share the payload decoder — a `VP8 ` or `VP8L`
body with its optional `ALPH` plane — which was factored out of `decode` as
`decode_payload`. The canvas starts fully transparent; the `ANIM` background
colour is advice to a viewer, not a colour the frames are composited onto,
which is also what libwebp does.

## The oracle is libwebp, not the incumbent

`image` 0.25.10 **ignores the ANMF dispose-to-background flag**. On the alpha
fixture its third frame keeps pixels libwebp clears: all 6912 pixels of the
canvas differ, and libwebp agrees with us on every one. So the fixture that
exercises blending and disposal is gated against libwebp's own composited
frames (written by `scripts/gen-still-fixtures.sh` through Pillow), and the
incumbent stays the oracle only for the opaque fixture, where the two agree.

ffmpeg cannot arbitrate: its WebP decoder refuses an animation outright
(`image data not found`).

## Gates

| Gate | Result |
| --- | --- |
| `webp_disposal_and_blending_match_libwebp` | 3/3 frames **pixel-exact**, delays 60/90/30 ms exact |
| `an_animated_webp_composites_frame_for_frame_like_the_incumbent` | 3/3 frames, max delta 3, ≥52.7 dB (lossy VP8 payload) |
| `an_animated_webp_is_refused_by_name` | still `decode` still refuses an animation |
| `an_animated_webp_survives_mutation` | 10 000 mutations of the animation path, no panic, no hang |
| `cargo test -p ec-image` | 25 + 13 + 9 + 2, 0 failed |
| clippy | no new lint from this lane (the 1.98 toolchain fires new lints repo-wide; that is a separate sweep) |

## New fixtures

`anim-alpha.webp` — three lossless frames carrying alpha, flags `0x2` (no
blend), `0x3` (no blend + dispose to background) and `0x0` (blend over what the
disposal left). The pre-existing `animated.webp` is opaque and disposes
nothing, so it left both of those paths untested.
`anim-alpha-f{0,1,2}.png` — libwebp's composited frames, the goldens above.
