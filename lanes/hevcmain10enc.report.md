# lane hevcmain10enc: HEVC Main 10 / HDR output from ec-h265

Branch lane/hevcmain10enc (from main 41075748), not pushed.

## Finding
Main 10 encode itself (generic 10-bit intra/inter, deblock tc/beta scaling, QP -12..51, rate control, SPS
profile_idc 2, I010 input) was already merged by lane h265m10enc (7b8e7fcd). SAO is not used by this
encoder, so there is no SAO path. This lane verified that work against the stricter bar and added what was
missing: HDR signalling, proof tests, measurements.

## Added
- `EncoderConfig::chroma_sample_loc: Option<(u8,u8)>` -> VUI chroma_loc_info (also parsed back).
- `EncoderConfig::mastering_display: Option<ec_h265_syntax::sei::MasteringDisplay>` -> full
  mastering_display_colour_volume SEI (primaries, white point, luminance); `sei::hdr_metadata_rbsp_with`.
  Previously only the luminance pair was possible with fixed BT.2020 primaries (still the fallback).
- Tests: `tests/enc8_bitstream_hash.rs` (8-bit bitstream hashes pinned: intra/GOP, QP/bitrate, 1/4 threads;
  captured on unchanged main, unchanged after); `tests/main10.rs` gained smooth gradient (QP -12..51),
  0/1023 black/white/checker extremes, rich content across the QP range (intra + GOP, decode == recon,
  recon in 0..1023), HDR VUI+SEI and defaults-absent tests.
- `scripts/hevc10-hdr-check.sh`: testsrc2/mandelbrot/gradients, PQ and HLG, full-range, intra+GOP: ffprobe
  reports profile=Main 10, pix_fmt=yuv420p10le, bt2020 primaries/space, smpte2084 or arib-std-b67, range,
  chroma_location=topleft; ffmpeg decode == encoder recon byte for byte. 12/12 pass (server host c).
- `examples/hevc_gop.rs`: `EC_H265_HDR=pq|hlg`, `EC_H265_FULL_RANGE=1`.

## Encoder config API for edith 10-bit/HDR export
```rust
let mut cfg = EncoderConfig::new(w, h);
cfg.bit_depth = 10;                       // input VideoFrame must be PixelFormat::I010
cfg.video_signal_type = Some(VideoSignalType {
    video_format: 5, video_full_range_flag: false,
    colour_description: Some(ColourDescription {
        colour_primaries: 9, transfer_characteristics: 16 /* PQ; 18 = HLG */, matrix_coeffs: 9 }),
});
cfg.chroma_sample_loc = Some((2, 2));     // optional
cfg.mastering_display = Some(MasteringDisplay {   // optional, units 0.00002 / 0.0001 cd/m2 per SEI
    display_primaries: [(13250,34500),(7500,3000),(34000,16000)],  // G, B, R
    white_point: (15635,16450), max_luminance: 10_000_000, min_luminance: 50 });
cfg.content_light.max_cll = Some(1000.0); cfg.content_light.max_fall = Some(400.0);
```
All of these default to absent (8-bit and 10-bit streams carry no extra VUI/SEI unless set).

## Measurements
- 8-bit hash: unchanged (pinned test passes; the 8-bit hot paths were not touched, only config fields and the
  VUI/SEI writers, so there is no 8-bit perf cost to measure beyond noise).
- BD-rate vs x265 veryfast `-tune psnr` bframes=0 keyint=8, QPs 22/27/32/37, 720p, 64 frames (yuv 6:1:1):
  | clip | 8-bit | 10-bit |
  |---|---|---|
  | testsrc2 | 17.32% | 14.49% |
  | mandelbrot | 29.01% | 25.65% |
  10-bit is 2.8 and 3.4 points better than 8-bit: bar (gap + 5) met.
- Speed, local, 1080p30 testsrc2, GOP 30, QP 28, 6 threads (EC_H265_THREADS=6), whole-process wall incl.
  raw read and recon write, host load average ~20 on 12 cores (other lanes), best of 8: 10-bit 2.31 s for 60
  frames = 26 fps = 0.87x realtime; 8-bit best 1.55 s = 39 fps. Under load; not a clean number and below the
  >= realtime bar: 10-bit runs on scalar u16 kernels where 8-bit has SIMD. Re-measure on a quiet host; a
  10-bit SIMD pass (transform/intra/deblock) is the follow-up if it stays under 1x.

## Gates
Rebased on main 33a6f2e5. Workspace clippy -D warnings clean; EC_REQUIRE_FIXTURES=1 cargo test -p ec-h265 -p ec-h265-syntax --release all pass (pre-rebase full run); after rebase clippy plus enc8 hash and main10 tests re-run, pass.
EC_REQUIRE_FIXTURES=1 cargo test -p ec-h265 -p ec-h265-syntax --release on server host c.
