# deep10: 16-bit samples end to end in 10-bit exports (branch lane-deep10)

Commits (local only): layered/adjustment at 10 bits; 10-bit tone map, turn, stretch, dissolve B; tests; clippy fix.
Tests: crates/engine/tests/export_deep_ten_bit.rs (14 cases), unit tests in tonemap.rs and stabilise.rs.

Metric note: the near-lossless encoder adds noise, so the distinct-luma count alone cannot show a staircase
(an 8-bit layered export still counted ~876 codes under HEVC). Each case therefore also asserts that no frame has
more than 60% of its luma on the 8-bit lattice (8-bit upshift lands 75-100%, true 10-bit ~25-39%), plus >256 codes
and frame-0 PSNR where the path should equal the source. AV1 (near lossless) is the test codec.

## Per path (before -> after), 640x360 diagonal ramp, 876 source codes
| path | before | after |
|---|---|---|
| plain | 10-bit (877 codes, PSNR inf) | same |
| grade / LUT | 10-bit (919 / 790 codes) | same |
| effects (single clip) | 10-bit (651 codes) | same |
| transform + crop | 10-bit (836 codes, 39% lattice) | same |
| dissolve | 10-bit when B 10-bit, untinted, unturned; else 8-bit B | always 10-bit (B widened if 8-bit source; turn/stretch/tone map at 10 bits) |
| layered composite (LayerStack, opacity, fades, per-clip grade/transform/effects, layered dissolve) | 8-bit: 83% on lattice, opaque-over-base PSNR 56 dB | 10-bit: 25% lattice, 877 codes, PSNR 75.5 dB |
| adjustment layer | 8-bit: 83% lattice | 10-bit: 25% lattice |
| alpha generators in a stack | 8-bit | composed at 10 bits (generator picture itself is 8-bit, widened) |
| title / text overlay | 10-bit (overlay narrowed, changed codes merged back) | same |
| speed ramp / slow | 10-bit (877 codes) | same |
| stabilise (layered) | 8-bit warp | warp generic over the sample, runs at 10 bits (test layers-stab) |
| tone map (HDR to SDR) | narrowed to 8 bits | new ToneMapper::map16 (10-bit grid nodes, lazy) |
| rotation / pixel-aspect stretch | forced 8-bit | place_picture16 turns/stretches at 10 bits |

## Still 8-bit
- Captions/titles themselves: text.rs overlay is 8-bit BGRA/I420 blended on a narrowed copy (burned16 writes back
  only changed codes, so the picture keeps its 10-bit samples); a 10-bit blend needs a wide overlay blend in text.rs (not touched, owned elsewhere).
- Generators that produce a gradient (their picture is 8-bit by construction; only widened).
- 8-bit sources and the 8-bit export path (unchanged, export_default_bytes green).
- Effects: all already generic over Sample (u8/u16); none stays 8-bit in a 10-bit export.
- Frame blending in the speed path: not 8-bit anywhere found (slow/ramp test: 877 codes).
- Hardware (DMA) encodes of untouched pictures are unchanged.

## Gate (server c, final tree)
- clippy --workspace --all-targets -D warnings: clean.
- cargo test --workspace --release --no-fail-fast: passed 3713, failed 3, ignored 61.
  Failures: the two known hw_midgop_seek tests, and avi `unreadable_raw_and_msmpeg4_v1_are_refused_by_name...`
  (Demuxer::open error text at avi.rs:498; demux only, untouched by this work; not verified against clean main).
  Stabilise flake did not occur. export_default_bytes, export_ten_bit, transitions, stabilise, export_color green.
