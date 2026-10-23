# Lane mpeg2v report: ec-mpeg2 (MPEG-1/2 video decoder)

Branch `lane/mpeg2v`, rebased on main `6acc9d93` (mp2a merged). Not pushed.

## Result
New crate `crates/ec-mpeg2`, pure Rust, depends only on ec-core. `CodecId::Mpeg2Video` added to ec-core
(one variant; registry count assertion is now 29). ec-mpegts exposes stream types 0x01/0x02 with it.

Byte-exact against ffmpeg 8.1.3 (default simple IDCT) on:
- ffmpeg-encoded matrix (`tests/ffmpeg_parity.rs`): progressive and interlaced IPB, MPEG-1, noise q1/q31, alternate scan,
  intra_vlc, non-linear quant, DC precision 8-11, 4:2:2, 720x576, 720x480, 1920x1080 (420 and 422), odd sizes, large MVs.
- 1200 random-syntax streams (`tests/synth_parity.rs`, generator in `tests/common/synth.rs`): field pictures, dual prime,
  16x8, field/frame DCT, matrices, 4:2:2, progressive_sequence, concealment MVs.
- Public streams from the FFmpeg FATE suite (`sony-ct3.bs`, `tcela-6.bits`, `hhi_burst_422_short.bits`): identical.
  Not committed (licence unclear). ISO 13818-4 streams were not available.
- Streams cut at a GOP start with a sequence header: identical.

Performance (one thread, release, 12-core desktop): 720x576 interlaced 6 Mbit: 586 fps (23x realtime);
1920x1080 interlaced 20 Mbit: 140 fps (5.6x at 25 fps). Scalar code only.

Robustness: `tests/robust.rs` (truncations, 300 corruptions, chunking invariance, huge dimensions) and fuzz target
`crates/ec-mpeg2/fuzz` (`decode_mpeg2`). 5 min on server b: 172,939 runs, no crashes, after fixing 3 real panics found by fuzz/corruption.

Gates: `cargo clippy --workspace --all-targets -- -D warnings` clean; `EC_REQUIRE_FIXTURES=1 cargo test -p ec-mpeg2 -p ec-mpegts -p ec-core --release` passes locally and on server b.
DONE.md has an ec-mpeg2 section.

## Refusals and known differences
- Refused by name: MPEG-1 D-pictures, scalable/SNR/spatial extension, 4:4:4 and monochrome, > 8192 wide or > 8192x4352 area.
- Pictures before the first sequence header are dropped (ffmpeg decodes them using probed parameters). A B picture with
  no forward reference is dropped (`dropped_pictures()` counts them).
- ffmpeg quirks reproduced: skipped B MBs predicted frame-wise; zero MV delta returns the predictor without range wrap.
- Not defined by the spec, not matched: MVs pointing outside the picture (ffmpeg skips the prediction), cbp 0 on coded MBs,
  IDCT overflow with extreme coefficients (ffmpeg's SIMD IDCT differs from the C one).
- Mid-row slice starts follow the spec; ffmpeg treats them differently. Not tested.
- Not done: SIMD, threading, edith wiring (`crates/app` untouched).

## API summary for the edith integration lane
```rust
use ec_core::registry::{CodecParameters, CodecId, Decoder};
let mut d = ec_mpeg2::Mpeg2Decoder::new(CodecParameters::new(CodecId::Mpeg2Video))?;
```
- `new(params)`: params.extradata, if any, is parsed as sequence header (+ extension). ec-mpegts supplies exactly that.
- Registry `Decoder` trait: `send_packet(Packet)` (each packet is complete start-code units, normally one picture; pts carried through),
  `receive_frame() -> VideoFrame` in display order, `flush()`, `reset()`. At most 64 frames are queued.
- Streaming API for raw elementary streams (.m2v): `send_bytes(&[u8], Option<i64> pts)`, `flush_stream()`,
  `pop_picture() -> Option<(VideoFrame, PictureInfo)>`, `sequence() -> Option<&SequenceInfo>`,
  `last_picture_info()`, `dropped_pictures()`.
- Output: planar `I420` (4:2:0) or `I422`, cropped to the coded size; pts is that of the packet holding the picture's
  picture header. Frames are reordered to display order (I/P output is delayed one reference, B immediately).
- Interlace: `VideoFrame` interlace flag is `Progressive`, `TopFieldFirst` or `BottomFieldFirst` (field pairs are woven).
- `PictureInfo`: `picture_type` (I/P/B), `temporal_reference`, `repeat_first_field`, `top_field_first`, `progressive_frame`,
  `field_pictures` (coded as two fields), `corrupt` (slice errors concealed), `mpeg1`, `closed_gop`.
  Frame rate comes from the sequence header, SAR from the aspect code (MPEG-2). RFF must be applied by the caller for display timing.
- ec-mpegts: streams of type 0x01/0x02 have `CodecId::Mpeg2Video`, one picture per packet, keyframe = sequence header or I picture.

## Commits (lane/mpeg2v)
CodecId::Mpeg2Video; ec-mpegts MPEG-1/2 video; decoder core; ffmpeg parity matrix; synth differential test + ffmpeg MV quirk;
fuzz target and robustness tests; DONE.md; chroma-format fix for repeated sequence headers. No trailers.
