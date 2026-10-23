# lane/proresfast report

Host: server b (AMD EPYC-Rome, 4 vCPU, shared; a few runs on host a were too loaded to use).
Bench: `cargo test -p ec-prores --release --test perf -- --nocapture` (1080p 422 HQ, noisy testsrc2, best of 10 passes of 24 frames).

## Decode speed (1 thread / 4 threads, fps)

| | 1 thread | 4 threads |
|---|---|---|
| before (main) | 44.5 | 133.9 |
| after | 63.0 - 65.6 (target 60 met) | 95 - 170 (host load varies; 169.9 best, 134 and up when quiet) |

4-thread figures are noisy because other jobs share the server; the 1-thread figure was stable (+-2 fps).
Output is bit-identical: all ec-prores tests, hash pins and ffmpeg parity tests pass.

What changed (ec-prores decode):
- Profiling by cycle counters: AC entropy decode was about 65% of a frame, IDCT about 12%, DC 5%.
- AC decode: 64-bit register bit buffer with branch-free refill (no per-coefficient load in the dependency
  chain), lookup tables hold code length, value and next-context offset (no clamps), three coefficients per refill.
- Two slices are decoded interleaved (`decode_slices`, `AcState`) to overlap the latency-bound chains.
- DC: 8-bit lookup tables per codebook.
- IDCT: explicit SSE2 `std::arch` implementation (x86_64 baseline, so no runtime detection is needed) with the
  `wide` code kept as the portable fallback on other targets; the DC-only shortcut is detected inside the IDCT, so
  the per-coefficient non-zero mask is gone. The unit tests compare against the scalar reference (200k blocks, 10 and 12 bit).
- Frame assembly: planes are written as little-endian bytes straight from the IDCT; a single-group progressive frame
  moves those buffers into the output (no row copy pass).
- `#![forbid(unsafe_code)]` became `#![deny(unsafe_code)]`; the only `allow(unsafe_code)` is the SSE2 module in idct.rs.
- AVX2 was not added: after the changes the IDCT is a small share and the entropy chain dominates.

## Encoder

- Interlaced (field) coding for every profile: a frame with `interlace` TopFieldFirst/BottomFieldFirst is written as two
  field pictures (interlaced scan, frame_type 1/2, per-field picture headers, one frame-wide quantiser search).
- 4444 and 4444 XQ alpha: `EncoderConfig::alpha_bits` (8 or 16) and `ProResEncoder::encode_with_alpha(frame, Some(&AlphaPlane))`
  (RDD 36 run-length alpha, 8-byte slice header, alpha bytes counted in the rate search). `encode()` writes opaque alpha.
- Encode speed was not touched (1080p HQ, 4 threads: 48.7 fps on host b).

## Checks (tests added in crates/ec-prores/tests/encode.rs, ffmpeg 8.1.3)

- `interlaced_every_profile_ffmpeg_and_ours_agree`: all 6 profiles, TFF and BFF, 331x75: ffprobe frame fields show
  interlaced_frame=1 and the right top_field_first; ffmpeg and ec-prores decode byte-identically; PSNR vs source > 34 dB;
  output independent of thread count. (Stream-level `field_order` stays "progressive" because the mov muxer writes no
  `fiel` atom; the order is in the ProRes frame header and in the decoded frames.)
- `interlaced_fields_are_coded_separately`: field-different content, 74.0 dB with field coding against 69.0 dB progressive.
- `alpha_4444_round_trips_and_ffmpeg_agrees`: 4444 and XQ, alpha 8 and 16 bit, progressive and BFF: ffprobe pix_fmt yuva444p12le,
  our decoder returns the alpha exactly (16-bit) or the replicated 8-bit value, ffmpeg and ec-prores agree byte for byte on
  Y/U/V/A, PSNR > 34 dB; no alpha plane gives fully opaque alpha.
- `alpha_config_is_validated`.
- Gate: `cargo clippy --workspace --all-targets -- -D warnings` clean; `EC_REQUIRE_FIXTURES=1 cargo test -p ec-prores --release` all pass.
- All media is synthetic (generated in the tests, ffmpeg testsrc2/mandelbrot).
