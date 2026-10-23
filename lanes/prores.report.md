# lane prores report

Branch `lane/prores` (worktree /home/user/.cache/wt/prores), one commit, not pushed, no trailers.

## Result
- New crate `crates/ec-prores`: Apple ProRes decoder. 422 Proxy/LT/Std/HQ -> `I210`; 4444/4444 XQ -> `I412`
  (new 12-bit 4:4:4 `PixelFormat::I412` in ec-core). Alpha (8/16-bit) as a separate 16-bit plane. Interlaced
  (TFF/BFF) woven with the interlace flag. All slice sizes, both scans, custom quant matrices, odd sizes.
- Byte-exact against ffmpeg 8.1 (`prores_ks`, `prores_aw`) on ~55 cases, 1 and 3 threads (IDCT is a bit-exact
  port of ffmpeg's `ff_prores_idct`, SIMD via `wide`, so no PSNR fallback).
- Gates on server host c: workspace `clippy --workspace --all-targets --release -D warnings` clean;
  `EC_REQUIRE_FIXTURES=1 cargo test -p ec-prores -p ec-mp4 -p ec-core --release` passes.
- Fuzz `decode_prores`: 10 min on server, 107k runs, no crash (seeded from 3 ffmpeg-made frames; corpus is gitignored,
  regenerate with the ignored `dump_seeds` test in `tests/robust.rs`).
- Performance, 1080p 422 HQ (957 KB/frame): single thread 34-40 fps (below the 60 target), 4 threads 65-79 fps.
  Why: ~1.5 M coefficients/frame through a bit-serial rice/exp-golomb decoder (~70% of time), `forbid(unsafe_code)`,
  baseline SSE2. Decoder defaults to slice-parallel over available cores (max 6); output is identical for any count.

## Public API (for edith)
- `ec_prores::ProResDecoder::new(CodecParameters)` (codec must be `CodecId::ProRes`, else `Unsupported`)
- `set_threads(n)` (1..=6; default = available cores, max 6)
- `decode(&[u8]) -> Result<VideoFrame>`: one ProRes frame (the mov sample); format I210 or I412
- `decode_full(&[u8]) -> Result<DecodedFrame { video, alpha: Option<AlphaPlane>, info: FrameInfo }>`
- `last_alpha() -> Option<&AlphaPlane>` (`width,height,stride,data: Vec<u16>`, 16-bit; 8-bit alpha is replicated v<<8|v), `last_info() -> Option<&FrameInfo>`
- `ProResDecoder::output_format(is_444)`
- implements the registry `Decoder` trait (send_packet / receive_frame / flush / reset; no reordering).
- Errors: `ec_core::Error::Unsupported` / `Corrupt` with names (version, chroma format, alpha type, slice size, size limits).
- ec-core: `CodecId::ProRes` ("prores", video), `PixelFormat::I412`. ec-mp4 demux: `apco apcs apcn apch ap4h ap4x` -> `CodecId::ProRes`.

## Notes
- Merge conflicts to expect with dnxhd/mjpeg lanes: `registry.rs` (enum variant, match arms, test list and the
  `all.len()` assert, now 30).
- Other exhaustive matches on `PixelFormat` across the workspace compile (clippy is clean workspace-wide).
- crates/app untouched. Tests use only synthetic ffmpeg-generated media, written to temp dirs at test time.
