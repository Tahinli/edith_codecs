# lane proresenc report

Branch `lane/proresenc` (worktree /home/user/.cache/wt/proresenc), not pushed, no trailers. Decoder files untouched.

## Result
- `crates/ec-prores/src/encode.rs`: Apple ProRes encoder. 422 Proxy/LT/422/HQ from I210 or I422 (8-bit widened), 4444 / 4444 XQ from I410 / I412 / I444. Progressive only; alpha off (`alpha_channel_type` 0); interlaced input is refused with `Unsupported` (documented, not done).
- Rate control: one quantiser for the whole frame (equal step is the MSE optimum), found by a two-level secant search over a slice sample; a Bresenham-spread fraction of slices takes the next finer step to land on the budget. Budget = Apple's 1080p29.97 rate (45/102/147/220/330/500 Mbit/s) scaled by `(macroblocks/8160)^0.846`, which reproduces Apple's UHD figures. Measured on dense content: 0.997-1.001 of nominal at 1080p (all six profiles) and 2160p (Lt, HQ, 4444).
- ffmpeg and ec-prores decode every profile identically (byte-exact); encoder output is independent of the thread count.
- Quality vs `prores_ks` (same profile, same input, 1080p): testsrc2 and mandelbrot 10-25 dB higher; noisy testsrc2 within -0.17..+1.5 dB (HQ +0.5, 4444 +1.5) at equal or smaller frames. (prores_ks wastes dB because its per-slice caps starve detailed slices.)
- Speed 1080p HQ on dense noise: 56.7 fps on server host a with 4 threads (test `encode_speed_1080p_hq`, gate with env `EC_PRORES_ENC_MIN_FPS=30`); single thread ~30 fps locally. Threads 1..=6 via `std::thread::scope`.
- Container: ec-mp4 muxer now writes a ProRes sample entry (`apco apcs apcn apch ap4h ap4x`, no config box, then `colr`, `fiel`, `pasp`). The fourcc comes from `CodecParameters.extradata` (4 bytes), which the encoder sets. ffprobe reads codec, profile, pix_fmt, `field_order=progressive`, colour tags, tag. ffmpeg itself writes vendor `FFMP` (not `apl0`) in the entry and `Lavc` as frame creator; this encoder leaves the entry vendor zero and writes creator `edth` (configurable).

## Encoder API (for edith export)
```rust
use ec_prores::{EncoderConfig, Profile, ProResEncoder, nominal_frame_bytes};
let mut cfg = EncoderConfig::new(Profile::Hq, width, height); // Proxy Lt Standard Hq P4444 P4444Xq
cfg.color = ColorInfo { primaries: 1, transfer: 1, matrix: 1, full_range: false }; // written to the frame header and params
// optional: cfg.threads (1..=6, default cores), cfg.bits_per_mb (rate override), cfg.qscale (fixed quantiser 1..=224), cfg.creator
let enc = ProResEncoder::new(cfg)?;
let bytes: Vec<u8> = enc.encode(&video_frame)?;   // one whole frame = one mov/mp4 sample
```
- Input `VideoFrame` formats: 422 profiles I210 / I422, 4444 profiles I410 / I412 / I444; size must equal the config; `interlace` must be Progressive. Planes may have any stride.
- Also implements `ec_core::registry::Encoder` (`send_frame` -> `receive_packet`, keyframe packets, pts carried, `codec_parameters()` has `extradata` = fourcc and the video params).
- Muxing: `Mp4Muxer::with_brands(w, *b"qt  ", &[*b"qt  "])`, `add_stream(StreamInfo::new(0, tb, enc.codec_parameters().clone()))` (set `width`/`height` in the video params; the encoder does), then `write_packet` with `flags.keyframe = true`. `Profile::fourcc()` / `Profile::from_fourcc()` map the sample entry.
- Output formats of a re-decode: 422 profiles I210, 4444 profiles I412 (12-bit); a 10-bit I410 source is scaled `<<2`.

## Tests (crates/ec-prores/tests/encode.rs, synthetic only)
ffmpeg-vs-ec-prores decode identical per profile (odd size 331x75, 2 frames); thread-count determinism; 8-bit widening and 4:4:4-into-422 refusal; frame sizes within +-10% of Apple 1080p (all profiles) and UHD (Lt, HQ, 4444) rates; Encoder trait + mov probe (ffprobe codec/profile/pix_fmt/field_order/colour/tag/frames, no config box, demux reads ProRes); PSNR vs prores_ks on testsrc2, mandelbrot, noisy testsrc2 (>= prores_ks - 0.5 dB); random-size round trip (120 cases, odd widths, 1 macroblock); speed. Example `examples/enc.rs` (raw yuv -> mov).

## Gates (server)
See the end of this file (filled in after the final run).
Final commit, server (hosts a/c): `cargo clippy --workspace --all-targets --release -- -D warnings` clean; `EC_REQUIRE_FIXTURES=1 cargo test -p ec-prores -p ec-mp4 -p ec-core --release` passes (existing decoder tests untouched and green; encode.rs 9/9); 1080p HQ encode 56.7 fps with 4 threads on host a.
