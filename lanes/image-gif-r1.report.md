# lane-image-gif r1 — GIF decoding in ec-image

## What landed

`crates/ec-image/src/gif.rs` (~600 lines): a GIF87a/GIF89a decoder written to the
spec — LSB-first LZW with the one-code-early width bump and the `cScSc` case,
sub-block chains, the four-pass interlace, graphic control extensions (disposal
method, transparent index, delay in centiseconds), local and global colour
tables, and frame compositing (blend, then dispose: method 2 clears the frame
rect to transparent, method 3 leaves the canvas as it was).

Wired into `lib.rs`: `ImageFormat::Gif`, signature guessing, `decode`, `info`,
and a new animation surface that every format answers:

```rust
pub struct AnimationFrame { pub image: Image, pub delay_num: u32, pub delay_den: u32 }
pub fn decode_animation(data: &[u8]) -> Result<Vec<AnimationFrame>>;
```

A still is one frame with a zero delay, so a caller that only wants to play
something never branches on the format.

One colour form is chosen for the whole animation: a single transparent pixel
anywhere makes every frame RGBA, so a caller walking the frames never has the
pixel layout change under it halfway through.

## Gates

| Gate | Result |
| --- | --- |
| `gif_decodes_pixel_exactly` | 6/6 fixtures (still, interlaced, transparent, odd size, animated, tiny) max delta **0** vs `image` 0.25.10 |
| `a_gif_animation_composites_frame_for_frame_like_the_incumbent` | frame count, per-frame delay (±0.5 ms) and every composited frame exact |
| `a_still_of_any_format_is_one_frame_of_animation` | PNG through `decode_animation` matches `decode` |
| `gif_survives_mutation` | 10 000 mutations, no panic, no hang |
| `a_gif_animation_survives_mutation` | 10 000 mutations of the animation path, no panic, no hang |
| `real_gifs_animate_like_the_incumbent` | **7/7 real GIFs on this machine, 454 frames, all exact** |
| `cargo clippy -p ec-image --all-targets -- -D warnings` | clean |
| `cargo test -p ec-image` | 11 + 8 + 2 + unit, 0 failed |

Real-library table (RGBA, bar 0):

```
Bonfire_zpsedocgnqr.gif        800x450  319 frames   9907.7 ms  exact
InfoLOTBD.gif                  617x623   48 frames   1858.6 ms  exact
dark-souls-bonfire.gif         172x236    9 frames     27.5 ms  exact
dcmbh13-…-7f0aa41b7895.gif     200x200   28 frames     84.5 ms  exact
download.gif                   768x384   27 frames    758.3 ms  exact
giphy.gif                      480x270    6 frames     86.2 ms  exact
wj8HNGQ.gif                    294x257   17 frames    145.3 ms  exact
```

(Timings are a debug build; no release perf claim is made.)

## Why this lane exists

gpui consumes `image`'s animation surface — `GifDecoder`, `AnimationDecoder`,
`Frame`, `Delay` — so replacing the registry `image` in edith needs a GIF
decoder underneath. The gpui-shaped half lives behind the `gpui` cargo feature
in `shims/image`, the way chrono keeps its serde impls behind one; `ec-image`
itself gains only `decode_animation`, which is a codec fact, not a gpui fact.

## Not in this lane

- BMP and TIFF (`ImageFormat` variants gpui names) — separate lanes.
- Animated WebP — separate lane.
- GIF *encoding* — nothing consumes it yet.
