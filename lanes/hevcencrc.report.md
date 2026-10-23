# hevcencrc: rate control for the ec-h265 sequence encoder

## Goal

DONE.md asks for bitrate control on the HEVC encoder, which had constant QP only. Target:
achieved bitrate within +-5% of target over a 60 s clip.

## Base

- Lane base: `927fb3db` (main at lane creation). The branch was then merged with main
  (`bdc74c55`, the AV1 splits and HEVC refusal work), so the verify runs are on the merged tree.

## What changed

- `crates/ec-h265/src/enc_rc.rs` (new): `BitRateControl`, a port of the H.264 frame-level loop
  (`crates/ec-h264/src/enc/rc.rs`) to HEVC. QP is 0..=51. The complexity model
  (`bits * 2^(QP/6)`, per I and P kind) predicts the QP that hits each picture's budget. A
  leaky bucket adds the correction for accumulated error. P pictures are coded at
  `qp + P_QP_DELTA` (2), as before. The per-row fast loop from H.264 is not ported (see below).
- `crates/ec-h265/src/encoder.rs`: `RateControl::Bitrate { kbps: u32, fps: f64 }`. A single IDR
  (`encode_idr`) gets its per-picture share, via the existing `TargetBits` path.
- `crates/ec-h265/src/enc_p.rs`: `encode_sequence` is now a wrapper over
  `encode_sequence_with(frames, sink)`, which streams frames in and pictures out, so a 60 s
  720p clip never has to sit in memory. Output bytes are unchanged for `ConstantQp`.
- `crates/ec-h265/examples/hevc_rc.rs` (new): streaming encode and decode driver.
  Decode feeds the stream one access unit at a time.
- `crates/ec-h265/tests/rate_control.rs` (new): 320x240, 150 frames, targets 1000/2500/5000 kbps
  within 5%, and identical bytes at 1, 2 and 4 threads.
- `scripts/hevc-rc.sh` (new): clip to 720p30 yuv, encode per target, decode with ec-h265 and
  with ffmpeg, `cmp` each decode against the encoder reconstruction, PSNR against the source.
- `enc_rc.rs` unit tests: convergence on a fixed-complexity model; QP stays in range.

## Controller iterations (measured)

| Attempt | Change | 60 s result at 1000 kbps |
|---|---|---|
| 1 | Bucket clamp +-8 pictures, correction at most +-2 QP | +21.79% (1217.9 kbps) |
| 2 (final) | Bucket clamp +-64 pictures, correction up to +-12 QP | +0.03% (1000.3 kbps) |

With the first version the integral term saturated at 8 pictures of debt, so a sustained
overshoot could not be paid back. The 2500 and 5000 kbps targets were already within 2% with
version 1.

## Test clip

- Source: `/home/user/Downloads/reelk.mp4` (1920x1080, 30 fps, phone recording),
  first 61 s copied without re-encode, then scaled to 1280x720 bicubic at 30 fps, I420.
  Frames: 1800 (60 s). Full-length, not the 20 s fallback.
- Config: keyint 30, default 4 worker threads on host b, `EncoderConfig::new(1280, 720)`.

## Results (final tree, 60 s, 1800 frames)

| Target kbps | Achieved kbps | Error | PSNR (ffmpeg avg Y/U/V, dB) | Mean QP | ec-h265 decode == recon | ffmpeg decode == recon |
|---|---|---|---|---|---|---|
| 1000 | 1000.3 | +0.03% | 46.44 | 28.16 | yes | yes |
| 2500 | 2500.8 | +0.03% | 54.43 | 11.66 | yes | yes |
| 5000 | 4999.5 | -0.01% | 59.29 | 4.18 | yes | yes |

Source: `scripts/hevc-rc.sh`, run on host b with `KEYINT=30 hevc-rc.sh <clip> 60 "1000 2500 5000"`,
log `~/.cache/hevcencrc-full2.log` on the controller (`exit=0`).

The PSNR is ffmpeg's `psnr` filter `average`, which weights Y, U and V together. Y-only PSNR
was not measured.

The low QP at 5000 kbps means the controller spends the budget on quality; the 720p
phone clip is cheap to code at these rates.

## Verify

| Check | Where | Result |
|---|---|---|
| RC integration test (`cargo test --release -p ec-h265 --test rate_control`) | host c | pass: 1003.2 / 2492.6 / 4995.5 kbps (+0.32% / -0.30% / -0.09%), identical at 1/2/4 threads |
| RC unit tests (`enc_rc`) | host c | pass (2 tests) |
| 60 s run (table above) | host b | pass, all six decodes match |
| `cargo test --release -p ec-h265 -p ec-h265-syntax` with `EC_H265_REF_YUV_DIR=/home/user/refyuv/hevcinter` | host a | exit 0; conformance `total: 9203 pictures checked, 9203 exact, 147/147 streams accepted` |
| `cargo clippy -p ec-h265 --all-targets -- -D warnings` | host (auto) | pass, exit 0 |

Setup note for the conformance run: `EC_H265_REF_YUV_DIR` must be an absolute path on host a
(`/home/user/refyuv/hevcinter`). With the unexpanded `$HOME` form, or with the variable
missing, the count is 7157 and 2046 pictures are skipped. This was seen in two earlier runs on
this lane and is not a code failure. A clean `main` checkout gives the same 9203 on host a.

## Known gaps

- The per-row fast correction from H.264 (`row_delta`) is not ported. The picture-level loop
  alone hit the 60 s target on this clip. Screen content, where bit cost falls off a cliff
  with QP, is untested and could need it.
- Only one natural clip was measured. The 5% bound is shown on 1000, 2500 and 5000 kbps for
  that clip, plus the 320x240 synthetic test.
- The single-IDR path (`encode_idr`) gets a per-picture budget only, with no feedback.
  `Bitrate` over a sequence is the path that meets the bound.
- The minimum QP is 0, per the spec range, not the H.264 floor of 10. The model does not
  pick QP below 4 on this clip, so the floor does not bind here.
