# Lane aacencbar: ec-aac encode INCUMBENT_BAR at 1–6 and 8 channels

The orchestrator wrote this report from the lane's final message.

**Bar.** The bar is the worst per-channel correlation of an ffmpeg decode of our ADTS output against an ffmpeg decode of the FLAC source. Lag is the best of 0, 1024 and 2048. The incumbent is rusty_aac 0.5.0. That crate refuses more than 6 channels, so the 8-channel bar uses ffmpeg's native AAC encoder as a stricter proxy.

All rows are 48 kHz at 64 kbps per channel.

| ch | layout | kbps | ours (worst) | bar | rate vs target | result |
|---|---|---|---|---|---|---|
| 1 | mono | 64 | 1.0000 | 0.9698 | +0.9% | MET |
| 2 | stereo | 128 | 1.0000 | 0.9743 | +0.9% | MET |
| 3 | 3.0 | 192 | 1.0000 | 0.9711 | +1.0% | MET |
| 4 | 4.0 | 256 | 0.9999 | 0.9645 | +0.6% | MET |
| 5 | 5.0 | 320 | 0.9996 | 0.9712 | +0.6% | MET |
| 6 | 5.1 | 384 | 0.9999 | 0.9648 | +0.9% | MET |
| 8 | 7.1 | 512 | 0.9996 | 0.9994 (ffmpeg native) | +0.6% | MET (margin 0.0002) |

**Fixes** (crates/ec-aac/src/encode.rs):
- `element_instance_tag` is now numbered per element type. 7.1 has two CPEs, and with both tagged 0 ffmpeg rejected the stream as a "duplicate" channel element.
- The ADTS header bits now count against each frame's budget. Before this, the rate overshot by 1.5–5.0%.

**Tests.** `encoder_meets_every_layout_bar`, which uses the LAYOUT_BAR table in tests/oracle.rs, and the existing `encoder_beats_the_incumbent_bar` both pass. Clippy is clean. scripts/gen-aac-layouts.sh generates the fixtures.

**Caveats.**
- The pure-sine fixtures saturate correlation, so this bar is weak evidence of perceptual quality.
- `sbr_real_library::chunk_a::full_chain_low_band_matches_own_core` fails. It reads a machine-local media file and was also seen failing on the base commit by the aacdecmulti lane.
