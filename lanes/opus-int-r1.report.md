# lane opus-int r1 — intensity band via libopus `equiv_rate` + `hysteresis_decision`

## What
- `celt_enc.rs`: the 1.0-era `effective_rate` ladder (8/12/16/18/19/20/100) is replaced by libopus 1.3.1 `equiv_rate = (bytes*8*50 << (3-LM)) - (40C+20)((400>>LM)-50)`, capped by the VBR bitrate, through the 21-entry `intensity_thresholds` / `intensity_histeresis` tables with per-encoder hysteresis state (`CeltEncoder::intensity`). One method `intensity_decision()` is called at both consumers (VBR target `coded_bins` and the stereo decision) so CBR frames get the decision too.
- Defect caught by the suite on the first cut: the decision only ran inside the VBR-target branch, so CBR frames kept `intensity = 0` (all bands mono): `encoder_roundtrips_at_every_rate` 2 ch 32 kbps corr 0.7989. Fixed by the shared method.

## Measured
- Intensity band (`short_block_bits_vs_libopus`, band start Hz): sadie@64 long ours 8160 → 5760 (ref 6720), short 8160 → 3840 (ref 5760); hein@64 long 6720 → 5760 (ref 5760), short 8160 → 5760 (ref 6720). Residual: the system ref (libopus 1.6 via ffmpeg) sits one band higher on long frames than the 1.3.1 formula yields at 64 kbps (needs bitrate ≥ 67k for band 15); the 1.5-era `celt_encoder.c` on disk has the same table/formula, so the offset is upstream of CELT (opus_encoder bitrate handed to CELT) and not verifiable without the 1.6 source.
- 14-row gate vs `lanes/opus-sp-r1.sweep.txt`: every row within ±.0001 corr (sadie@64 +.0039 → +.0038, zaur@64 −.0017 → −.0018); RATE ±5% pass (max −1.8%); DROPOUT 0. Intensity band position is corr-neutral at 64/96 kbps on this library.
- Suite: `cargo test -p ec-opus --release` 34 + 27 passed, 9 ignored.
