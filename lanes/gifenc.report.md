# gifenc: animated GIF encoder

## Outcome

`ec-image` gains `gif_encode` (`crates/ec-image/src/gif_encode.rs`, `pub mod gif_encode;` in lib.rs next to `png_encode`). It writes GIF89a with one global header, an optional NETSCAPE2.0 loop extension, and per frame a Graphic Control Extension plus an image with a local colour table. Per-frame palettes come from median cut, with optional Floyd-Steinberg dithering. Unchanged pixels are encoded as transparent, and each frame is cropped to the bounding box of its changed pixels. A frame with no change folds its delay into the previous frame. The module has no unsafe code and no new dependencies.

## API

```rust
pub struct GifOptions { pub loop_count: Option<u16>, pub max_colors: u16, pub dither: bool }
impl Default for GifOptions  // loop_count: None, max_colors: 256, dither: false
pub struct Encoder { .. }
impl Encoder {
    pub fn new(width: u16, height: u16, opts: GifOptions) -> Result<Self>;
    pub fn push_rgba8(&mut self, pixels: &[u8], delay_cs: u16) -> Result<()>;
    pub fn finish(self) -> Result<Vec<u8>>;
}
```

`loop_count`: `None` writes no NETSCAPE extension (plays once); `Some(0)` loops forever; `Some(n)` writes `n` as the repeat count.

## Tests

Command: `cargo test --release -p ec-image gif_encode` on host a.

- 8 passed, 0 failed (`gif_encode::tests`), 37 other lib tests filtered out.
- `cargo clippy -p ec-image --all-targets -- -D warnings`: no errors or warnings.

The eight tests:
- `a_palette_that_fits_is_reproduced_exactly`: 256 distinct colours, dither off, decoded pixels equal input.
- `a_changing_sequence_decodes_frame_for_frame`: frame count, delays (5, 16, 3 after merging an identical frame), and exact pixels for every frame.
- `transparent_pixels_stay_transparent_and_unchanged_ones_keep_the_canvas`: alpha 0 for transparent pixels, unchanged pixels keep the canvas.
- `the_loop_extension_is_written_only_when_asked`: no NETSCAPE for `None`; loop bytes for `Some(0)` and `Some(3)`.
- `photographic_content_stays_within_a_small_error`: mean abs error below 6 per channel, dither off and on.
- `a_static_frame_repeated_ten_times_costs_little_more_than_one`: size under 1.5x one frame, and decodes to one frame with delay 100.
- `bad_arguments_are_refused`: zero size, max_colors outside 2..=256, wrong buffer length, finish with no frames.
- `ffprobe_reads_the_output_when_it_is_installed`: decodes with ffprobe when it is installed and checks 8,4; the test passes without ffprobe (skipped). Whether ffprobe was present on host a was not recorded.

## Sizes

96x96 gradient-with-noise frame, 10 ms delay, max_colors 256:

| Case | Bytes |
|---|---|
| 1 frame, no dither | 6851 |
| 1 frame, dither | 7503 |
| 10 identical frames, no dither | 6851 |
| 64x64 crop, 1 frame | 4379 |
| 64x64 crop, 10 identical frames | 4379 |
| 10 frames, each changes 1/10 of the pixels | 39751 |
