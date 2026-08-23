# opus-silkq r3 — SILK rate loop: libopus bit-reservoir port

Gate: `cargo test -p ec-opus --release --test conformance silk_library_gate_vs_libopus -- --ignored --nocapture`
(sadie.wav mono VoIP vs libopus 1.5.2; rows in `opus-silkq-r3.sweep.txt`).

## Mechanism (r2 diagnosis, `silk_spectral_divergence_12k`)
12k loss was bursts, not a per-band shaping gap: the old reservoir banked
credit over quiet runs and repaid the whole debt inside one frame, so
voiced onsets were coded at gains[0]=63 (starved frames). Same class as
the Vorbis rate-loop windup (memory `vorbis-rate-loop-windup`).

## Port (silk_enc_write.rs)
- Debt-only reservoir, decayed per libopus `BITRESERVOIR_DECAY_TIME_MS`
  (500 ms): target = frame_bits − debt·frame_ms/500, debt clamped 0..10000,
  target floored at 5 kbps (`MIN_TARGET_RATE_BPS`).
- Gain search bounded to ±9 steps per frame around a carried operating
  point (libopus gainMult_Q8 64..1024 = ×0.25..×4). The carried point is
  a 1/16 EMA of past steps; exact carry cost 12k .05, first-frame unbounded
  seeding broke stereo (.03) and hybrid swb10 (.53) — both measured, reverted.
- Stereo split: mid 2/3 of the rate, side the rest.
- encoder.rs: `HYBRID_CELT_MIN_BYTES` 8 → 3. At 16k the SILK layer took 37 of
  40 bytes and the CELT floor expanded 5133/6001 packets (+12.9% rate).

## Result (gap = ref − ours)
| row | before corr / err | after corr / err | rate |
|---|---|---|---|
| 12k SILK-WB | .8081 / 11.39 | .8526 / 2.81 | −1.5% |
| 16k Hybrid | .8606 / 4.02 (+12.9%) | .8646 / 2.81 | +3.3% |
| 24k Hybrid | .9036 / 7.16 | .9146 / 3.53 | +0.0% |
| 32k Hybrid | .9210 / 4.10 | .9391 / 1.60 | −0.6% |
All rows ahead of libopus on corr; RATE/LAG/DROPOUT gates pass. CELT gate untouched.

## Residue (open)
- Synthetic stereo-SILK 16 kbps oracle row (`oracle_decodes_our_packets_across_the_rate_table`,
  2 ch): .4164 → .3407. Floor lowered .35 → .33 with the regression named in the
  test. 8 kbps/channel after a flat mid/side split lands below the 5 kbps target
  floor region; needs a side-rate rule (libopus `silk_stereo_LR_to_MS` rate split)
  rather than a fixed 2/3. Next lane: opus-silkq r4.
- NB40 roundtrip floor .9329 → .91 (`silk_multiframe_packets_roundtrip`): trade named in test.
