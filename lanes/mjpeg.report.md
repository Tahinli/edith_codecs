# Lane mjpeg report

Branch `lane/mjpeg` (worktree /home/user/.cache/wt/mjpeg). Not pushed. No trailers.

## What was built

- `ec-image::jpeg::decode_yuv(data, Limits) -> YuvImage` (crates/ec-image/src/jpeg.rs): JPEG straight to native planes (gray, 4:2:0, 4:2:2, 4:4:4), tightly packed, cropped, no upsampling, no RGB. Uses libavcodec's `simple_idct` constants and the DC level shift (1024 on the dequantized DC), so output is byte-identical to ffmpeg's `mjpeg` decoder. Annex K default Huffman tables are preloaded as tables 0/1 (DHT overrides). The stills `decode()` was refactored onto a shared `parse()` but behaves the same; ec-image tests are unchanged and pass.
- New crate `ec-mjpeg` (crates/ec-mjpeg): `MjpegDecoder` (ec-core `Decoder` impl, plus `decode(&[u8], Option<Timestamp>) -> VideoFrame`) and free fn `decode_frame`. Chose a small crate over a module in ec-image so ec-image stays "still images" and the codec has the same shape as ec-mpeg2.
- `CodecId::Mjpeg` (name "mjpeg", video) in ec-core registry (one variant, three match lines, test list entry, `all.len()` 29 -> 30 in the registry test: expect a trivial merge conflict on that number with prores/dnxhd).
- ec-mp4: sample entries `jpeg` (Photo JPEG) and `mjpa` map to Mjpeg. `mjpb` (MJPEG-B) is not mapped and shows up in the existing unsupported-entry listing (different packet layout: no SOI, custom markers; not cheap). ec-matroska: `V_MJPEG` maps to Mjpeg (demux only; I did not add mux).

## Behaviour

- Full range always (`ColorInfo.full_range = true`, matrix defaults to 5 / BT.470BG as ffmpeg labels it, container's matrix kept if stated).
- Two-field packets (two JPEGs in a packet): woven into one frame, `interlace` = TopFieldFirst / BottomFieldFirst. Mirrors ffmpeg: a packet is treated as two fields unless the container height equals the JPEG height (then only the first image is decoded). Field order: ffmpeg puts the first image on the top lines for mov/mkv and on the BOTTOM lines for AVI with the `MJPG` tag. `MjpegDecoder::set_field_order()` defaults to TopFieldFirst; an AVI caller must pass BottomFieldFirst; use the demuxer's `field_order(stream)` (`fiel` atom / Matroska) where it exists.
- Restart markers and progressive (SOF2) decode (ec-image already supported them) and are byte-exact vs ffmpeg.
- Refused by name (`Error::unsupported`): 12-bit precision ("12-bit JPEG"), arithmetic/lossless/hierarchical SOFn, CMYK/YCCK, 2-component, RGB-coded (Adobe transform 0 or ids RGB), 4:4:0 / 4:1:1 / 4:1:0 layouts (no PixelFormat). The layout check runs at the SOF, before any scan is decoded.

## Exactness

Byte-exact vs `ffmpeg -c:v mjpeg ... -f rawvideo` in native format, both with ffmpeg's default (SIMD) idct and with `-cpuflags 0` (spot-checked); no PSNR fallback needed. Tests (crates/ec-mjpeg/tests/ffmpeg_parity.rs, ffmpeg is the run-time oracle, nothing stored but the synthetic inputs):
- 15 Pillow-made stills (tests/data, regenerate with gen.py): restart markers, progressive, progressive+restart, odd sizes, 4:2:0/4:2:2/4:4:4/gray, optimized Huffman.
- video matrix: yuvj420p/422p/444p x sizes (64x48, 50x38, 51x37, 160x96) x q 2..31 x huffman default/optimal x mov/mkv, packets read through our ec-mp4 / ec-matroska demuxers.
- gray in mov and mkv (ffmpeg's encoder has no gray, so gray comes from Pillow JPEGs stream-copied).
- AVI1/MJPG packets with the DHT stripped (all four formats) vs ffmpeg decoding the same AVI, and vs the with-DHT decode.
- two-field packets in AVI (bottom-first), mov `mjpa` and mkv (top-first), 4 geometries.
- refusals by name.
Note: `-flags +ildct` is rejected by ffmpeg's mjpeg encoder ("interlacing not supported by codec"), so interlaced sources are built with `separatefields` (one JPEG per field) instead.

## Gates

- `cargo clippy --workspace --all-targets --release -- -D warnings`: clean (server host c, first commit).
- `EC_REQUIRE_FIXTURES=1 cargo test -p ec-image -p ec-mp4 -p ec-matroska -p ec-core -p ec-mjpeg --release`: all pass on the final commit (server host c).
- Fuzz: new `crates/ec-mjpeg/fuzz` target `decode_mjpeg` (decode_frame with steered container height and field order, plus decode_yuv); the existing ec-image `jpeg` fuzz target also calls `decode_yuv` now. Run on server host c, 601 s, 468,686 executions, no crash, no artifacts (seed corpus: the fixtures, single and doubled for the two-field path).

## AVI demux status

`ec-riff` has `avi.rs` (752 lines): `AviReader` reads stream headers and `movi` chunks (including `rec ` lists, idx1 and seeking), but it is audio-only by design: `AviAudioStream` only, `strh`/`strf` parsed for WAVEFORMAT, picture chunks skipped (`AviPacket` is audio). A video demuxer would take roughly 250-400 lines in ec-riff: parse `strh` type `vids` (handler fourcc, scale/rate, length) and `strf` BITMAPINFOHEADER (width, height, compression fourcc, extradata), map fourcc `MJPG`/`mjpg`/`AVRn`/`dmb1`/`jpeg` etc. to CodecId::Mjpeg, return `00dc`/`00db` chunks as video packets with the existing index/seek machinery (keyframe flag from idx1 0x10), build `StreamInfo` with frame rate from scale/rate; plus OpenDML `ix##`/`indx` super-index for files over 1 GB (extra ~100 lines) and tests. The MJPEG-specific piece is already here (default tables, field order BottomFieldFirst for the `MJPG` tag).

## Public API summary for the edith integration

```rust
// crate ec-mjpeg
pub struct MjpegDecoder;                      // impl ec_core::registry::Decoder
impl MjpegDecoder {
    pub fn new(params: CodecParameters) -> Result<MjpegDecoder>;   // params.codec must be CodecId::Mjpeg
    pub fn set_field_order(&mut self, order: Interlace);          // default TopFieldFirst; AVI MJPG tag => BottomFieldFirst
    pub fn set_limits(&mut self, limits: ec_image::Limits);
    pub fn decode(&mut self, packet: &[u8], pts: Option<Timestamp>) -> Result<VideoFrame>;
}
pub fn decode_frame(data: &[u8], container_height: u32, first_field: Interlace, limits: Limits) -> Result<VideoFrame>;
// crate ec-image
pub fn ec_image::jpeg::decode_yuv(data: &[u8], limits: Limits) -> Result<YuvImage>;  // + Subsampling, YuvPlane, YuvImage
```
Output `VideoFrame` formats: `Gray8`, `I420`, `I422`, `I444`; `color.full_range == true`; `interlace` Progressive or Top/BottomFieldFirst. Registry id `CodecId::Mjpeg`. Via the trait: `send_packet` then `receive_frame` (one frame per packet, no delay).

Caveats for integration: (1) matrix is reported as 5 (BT.470BG, same coefficients as 601) unless the container states one; (2) frames decoded from two-field packets have height = 2 x JPEG height; (3) the decoder does not sniff Adobe/EXIF orientation (video).
