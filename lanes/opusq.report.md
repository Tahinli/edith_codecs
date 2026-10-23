# lane opusq: ec-opus encoder quality vs libopus

Signals: synthetic music (6-note chords with harmonics, amplitude-modulated noise, decaying noise bursts every 250 ms),
8 s, 48 kHz. c1 = quiet noise floor, c2 = noisier, k2 = correlated stereo. ffmpeg `-c:a libopus -application lowdelay
-frame_duration 20 -vbr off` (CBR, CELT, complexity 10) vs ec-opus CBR. SNR is the L/R pair after pre-skip alignment.
Both streams are the same size (within 0.3%). Numbers are L SNR in dB; "lsd" is mean band-wise log-spectral distance (lower is better).

| signal @ kbit/s | ec before | ec after | libopus | gap after | lsd after / libopus |
|---|---|---|---|---|---|
| c1 64 | 15.13 | 15.49 | 15.77 | -0.28 | 3.17 / 3.59 |
| c1 96 | n/m | 17.81 | 18.61 | -0.80 | 2.73 / 2.96 |
| c1 128 | 19.86 | 20.49 | 21.30 | -0.81 | 2.19 / 2.57 |
| c1 256 | 25.64 | 26.12 | 26.55 | -0.43 | 1.44 / 2.04 |
| c2 64 | n/m | 12.85 | 12.88 | -0.03 | 2.57 / 2.93 |
| c2 96 | n/m | 14.64 | 14.94 | -0.30 | 2.08 / 2.33 |
| c2 128 | 15.93 | 17.28 | 17.61 | -0.33 | 1.74 / 2.02 |
| c2 256 | n/m | 21.43 | 21.56 | -0.13 | 1.16 / 1.54 |
| k2 64 | 13.31 | 13.67 | 13.73 | -0.06 | 2.28 / 2.51 |
| k2 96 | n/m | 15.17 | 15.40 | -0.23 | 1.80 / 2.00 |
| k2 128 | 17.28 | 18.38 | 18.61 | -0.23 | 1.47 / 1.79 |
| k2 256 | n/m | 21.40 | 21.45 | -0.05 | 1.07 / 1.41 |
| 5.1 (6 indep. ch) 256, mean of 6 | n/m | 16.79 | 17.20 | -0.4 | 2.64 / 2.81 |

(n/m = not measured before; the before values are from the first measurement runs. c2@128 went 15.93 -> 17.28 from the
trim change alone, 17.22 -> 17.28 with leak + tonal trim.) Worst gap is 0.81 dB; target was 1.5 dB.
All three ec streams decode without a message under `ffmpeg -c:a libopus` (the libopus decoder).

## Cause and change

Diagnosis: per-frame SNR gap was uniform (not transient frames); mono was at parity, stereo was not. A temporary
decoder dump of each frame's trim/dynalloc/pulses showed libopus giving more bits to the low, tonal bands
(trim 5 vs ec 4, plus dynalloc boosts on bands 0-1).

1. `alloc_trim_analysis` used libopus 1.2's discrete trim ladder. The continuous trim (libopus 1.3+: stereo correlation
   term `0.75*log2(1.001-xc^2)`, spectral tilt, `-2*tf_estimate`) existed behind `EC_OPUS_TRIM13` and was off.
   Now on by default (`EC_OPUS_TRIM13=0` restores the ladder). This is +0.4 to +1.3 dB, the whole of the SNR gap.
2. `EC_OPUS_LEAK` (analysis leakage boost on the low bands, matches libopus's dynalloc offsets exactly in the dumps)
   and `EC_OPUS_TRIM_TONAL` are now on by default (`=0` disables). SNR +0.05..0.1 dB, but lsd drops 3.5 -> 2.2 on c1@128,
   now better than libopus. Cost: the tonality analysis runs by default (about 25% of encode time, per the old comment).
3. New test `crates/ec-opus/tests/snr_floor.rs` (0.3 s in release): stereo 64/96/128/256 kbit/s and 5.1 256 kbit/s
   SNR floors on a correlated music-like signal, checked through ec's own (libopus-exact) decoder. The old defaults
   fall under the 64/96/128/256 floors, so a revert shows.

## Where the rest of the gap is

libopus complexity ladder on c1@128 (SNR): c0-c4 20.2-20.4 (= ec before/after trim), c5 pitch prefilter 20.8, c7 analysis
21.0, c8 stereo theta RDO 21.3. ec now sits between c4 and c7. Remaining 0.3-0.8 dB is the pitch prefilter/postfilter
(not implemented, stated in celt_enc.rs docs) and `theta_rdo` (not implemented). Not touched: not needed for the 1.5 dB target.
Other items from the checklist (spread, tf_analysis, dynalloc, coarse energy, intensity/dual thresholds, CBR byte use) were
checked and are libopus ports with matching decisions; two-pass coarse energy (`EC_OPUS_TWO_PASS=1`) gives +0.02..0.06 dB, left off.

## Gates

- `cargo clippy -p ec-opus --all-targets -- -D warnings`: clean.
- `cargo test -p ec-opus --release` (server host c): 28 passed, 1 failed in conformance, `decode_speed` (a throughput floor on
  the decoder fixtures, which this change does not touch; it ran while the server was loaded); it fails identically (93-98x vs 104x floor) on unmodified main 01165cf2 on the same server, so it is a host-speed issue, not this change.
- snr_floor: 2 passed.
