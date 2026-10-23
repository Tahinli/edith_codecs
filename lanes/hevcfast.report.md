# lane hevcfast: ec-h265 speed presets

## Result: 30 fps at 1080p 10-bit NOT reached (Faster 16.5, Fast 13.9, Default 11.3 fps, host c under load 7). Fast meets the BD-rate target.

## API
```rust
use ec_h265::encoder::{EncoderConfig, Preset};
let mut cfg = EncoderConfig::new(w, h);
cfg.speed = Preset::Fast; // or Preset::Faster; Preset::Default (the default) is bit-identical to before
```
`hevc_gop` example: `EC_H265_PRESET=fast|faster`. Applies to P pictures (`keyint > 1`); IDR path unchanged.

## What the presets change (P-picture search only, `PKnobs::for_preset` in enc_p.rs)
- Fast: horizontal/vertical halves searched only for 32x32 (not 16), merge candidates costed max 3, one AMVP flag (smaller mvd), intra modes considered 3, intra skipped when inter cost/sample < 6, integer scan range 8/4 (gate 12), sub-pel skipped when SAD/sample < 1, wider early skip/split thresholds (16 / 1.0).
- Faster: Fast plus no halves, no 8x8 CUs (except picture edges), merge max 2, intra modes 2, no quarter-pel step, full scan range 4/0, thresholds 32 / 2.0.
- `EC_H265_*` env vars (MERGE_MAX, MIN_LOG2, HALVES_MIN, INTRA_MODES, SUB_GATE, NO_QUARTER, AMVP_BOTH, ...) override any knob for sweeps.

## BD-rate vs Default (yuv 6:1:1 PSNR, QP 22/27/32/37, 720p 10-bit, 32 frames, keyint 8, 4 threads; positive = more bytes)
Clips (synthetic, lavfi): a = testsrc2 slow scroll + noise, b = mandelbrot zoom, c = gradients + noise.
| preset | a | b | c | CPU ms/frame a / b / c (default 246 / 208 / 804) |
|---|---|---|---|---|
| Fast | +2.6% | +2.9% | -0.4% | 180 / 115 / 652 |
| Faster | +26.6% | +26.6% | -2.5% | 103 / 81 / 385 |

## fps (1080p testsrc2 30 frames, keyint 8, QP 30, 4 threads, host c 4 vCPU, load 7-8 from other lanes, so noisy)
| depth | Default | Fast | Faster |
|---|---|---|---|
| 8-bit | 10.2 | 16.6 | 20.4 |
| 10-bit | 11.3 | 13.9 | 16.5 |
Unloaded Default is 13-15 fps (hevc10simd2), so expect roughly 1.3x (Fast) and 1.8-2x (Faster) of that: about 18-20 and 25-28 fps.
Single-thread profile (Faster, 1080p 10-bit): IDR pictures are still 25% of CPU (0.23 s each, one per keyint), code_cu 1.7 s of 2.7 s P time, ME 0.3 s, ref half-pel planes 0.33 s.

## Verification
- Default output hashes unchanged: 8-bit 29fc16181fe9, 10-bit e8be339172d4.
- Fast and Faster streams, 8 and 10-bit 1080p: ffmpeg decode is bit-exact against our recon (cmp).
- Gate (host c): clippy --workspace --all-targets -D warnings clean; EC_REQUIRE_FIXTURES=1 cargo test -p ec-h265 --release all pass (run before a doc-comment and rustfmt-only fix; clippy and the 10-bit hash rechecked after).

## Not done / next
- Faster loses ~27% on content with real texture; 8x8 CUs and halves matter. Needs a cheaper IDR (coarser intra pre-screen), SIMD for half-pel plane fill, MC, and the intra SAD ranking to approach 30 fps.
- Noisy residual-dominated content (clip a) gains little from search cuts; the time is in transform/CABAC of the chosen candidate.
- Scripts: scripts/hevc-speed-sweep.sh (BD + CPU per label), scripts/hevc-preset-quick.sh.
