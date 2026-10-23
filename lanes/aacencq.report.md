# lane aacencq: AAC-LC 128k stereo correlation finding

Base: `1cd59b57` (main), branch `lane/aacencq`.

## Goal

Explain why `ec-aac` AAC-LC at 128k stereo scores correlation 0.974786 on the bench content
(lanes/benchaudio.report.md, finding 1) while the oracle's INCUMBENT_BAR test passes. Fix the
encoder if it is a defect, or fix the bench if it is an artifact.

## Verdict

Not a measurement artifact, and not a gain, scale, alignment or band-limit bug. The bench
measures correctly. The gap is a rate-allocation outcome of the psychoacoustic model on
noise-bearing content. It is not a one-line defect, so no encoder change is made in this lane.
The oracle passes because its content is far easier than the bench's, not because the encoder
is good on the bench's content.

## Checks run (local, ffmpeg-decoded, bench's own binary `~/.cache/benchaudio-bin/ec-aac-bench`)

1. Alignment. Best lag is 1024 samples (one frame, the encoder's own delay). ADTS has no edit
   list, so the lag is inherent and the bench's +-4096 search is not the cause.
2. Gain and scale. Pure sines give gain 1.0000 and SNR 47 to 64 dB at lag 1024 (220, 1k, 3k,
   8k, 100 Hz). The bench mix's least-squares gain is 0.9135, and its RMS is 1.067 times the
   source's. This is a 9 percent overshoot, which I could not reproduce as 1.04. No scalefactor
   or global-gain error shows up in the tone-only tests.
3. Channel mapping. Per-channel results are symmetric (ch0 0.974794, ch1 0.975267). The
   interleave and s16 path are fine.
4. Spectral breakdown of the mix (lag 1024). Almost all error is at the tone bins: 217, 223,
   281, 328, 334 Hz sit at about 30 dB against tone peaks near 52 dB, about -22 dB relative.
   The noise bands sit at 2 to 6 dB SNR.
5. Tone-only control (no noise). The same tone set gives SNR 45 dB and corr 0.999984 at lag
   1024. The tones themselves are coded accurately.
6. Noise-level sweep, same tones, 128k stereo, lag 1024:

   | pink-noise amplitude | corr | SNR (dB) |
   |---|---|---|
   | 0 (tones only) | 0.999984 | 45.0 |
   | 0.0003 | 0.999961 | 41.1 |
   | 0.003 | 0.999587 | 30.8 |
   | 0.03 (bench) | 0.974451 | 13.0 |

7. Bitrate sweep, bench mix: 64k 10.2 dB, 128k 13.0 dB, 256k 18.3 dB, 384k 23.8 dB.
   The ADTS size matches the target exactly, so the rate loop fills the budget every time,
   including the tone-only case.

Mechanism. `write_frame` (crates/ec-aac/src/encode.rs, outer rate loop about line 560) finds
the finest common offset that fits the bit budget. The psychoacoustic threshold
(`psychoacoustics`, about line 421) allows tonal bands about -24 dB of noise (SMR 24 dB) and
noise-like bands about -6 dB (SMR 6 dB). At 128k the quiet pink noise (rms 0.006, -44 dBFS, but
still above the relative `ATH_FLOOR` of 1e-7) costs enough bits that the common offset settles
where the tonal bins sit near their -22 dB threshold. Native ffmpeg AAC keeps the same tones
at 38 dB on this content, so its model spends the budget differently. That is a psychoacoustic
and allocation difference, not a bug in the quantiser or inverse mapping.

## Why the oracle passed and the bench did not

- `crates/ec-aac/tests/oracle.rs` `INCUMBENT_BAR` (line 400) uses `flac-*-48000.flac` fixtures
  from `scripts/gen-fixtures.sh`. Those are pure sine tones (`FREQS=(220 440 660 55 880 1320)`,
  3 s). The stereo 128k row gives corr 0.99997 at lag 1024. The bar is 0.9743, the incumbent's
  number on those same sines.
- `LAYOUT_BAR` (line 470) has the same content and the same margin. Its 5.1 and 7.1 rows
  also clear by 0.03 or more.
- The bench uses tones plus independent pink noise over 60 s, which is a harder content type.
  The bar was never measured on it, so it cannot catch this gap.

The oracle is therefore correct for what it tests but weak as a quality gate: its content
cannot distinguish a good noise-bearing allocation from a poor one.

## Numbers (host b, unchanged tree)

- `cargo test --release -p ec-aac`: all test binaries ok. Counts: 48, 4, 1, 1, 2, 1, 14 (1
  ignored), 1, 1, 16 passed, 0 failed.
- `cargo clippy -p ec-aac --all-targets -- -D warnings`: clean.
- Oracle encoder tests (`--test oracle encoder_`): 3 passed. Per-channel corr is 0.9999 to
  1.0000 across mono 96k, stereo 128k and 192k, and 5.1 384k and 448k. The bars are 0.9846,
  0.9743, 0.9864, 0.9648 and 0.9611 respectively.
- `encode_to_adts` and `encoder_meets_every_layout_bar` pass. Both use the same sine fixtures.

## Before and after

No encoder source change, so before and after are identical:

| content | 128k stereo corr (min channel) |
|---|---|
| oracle sine fixture `flac-stereo-48000.flac` | 0.99997 (bar 0.9743) |
| bench tones plus pink noise, 60 s | 0.974786 (ffmpeg native 0.999845, libfdk 0.999937) |

## What is left

- The encoder defect is real on noise-bearing content: at 128k it gives 13 dB SNR where ffmpeg
  native gives about 38 dB. This needs a psychoacoustic or rate-allocation change (for example a
  noise-band threshold or budget-share rule). Any such change moves INCUMBENT_BAR and
  LAYOUT_BAR and needs its own lane with a new bar measured on noisy content.
- Recommended next step: add a fixture with tones plus low-level noise (generated with ffmpeg,
  as in the bench) and gate it at the measured value with a note. Keep the sine bars as they are.
- The bench report's gain figure of 1.04 does not match my measured 0.9135 optimal gain. The
  bench row's numbers should be re-checked before the figure is quoted.
- No bench rows were changed in this lane.
