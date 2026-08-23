# opus-silkq r4 — stereo SILK mid/side rate split

Ported the libopus 1.5.2 `silk/stereo_LR_to_MS.c` rate logic into
`SilkStereoEncoder`, replacing the static mid = 2/3 bps split that r3's
bit-reservoir port had left regressing the synthetic stereo row.

## What was ported (`SilkStereoEncoder::stereo_rates`, silk_enc_write.rs)

- Overhead subtraction: `total = bps − (is10ms ? 1200 : 600)`, clamped ≥ 1.
- `min_mid = 2000 + 600·fs_kHz` (NB 6800, MB 9200, WB 11600).
- Split: `mid = 8·total/(13 + 3·frac)`; below `min_mid` the mid is clamped and
  the width drops as `4(2·side − min_mid)/((1 + 3·frac)·min_mid)`, clamped [0, 1].
- Width smoother: `smth += (width − smth)·0.01` each frame, state kept across
  packets (`smth_width` init 1.0, `width_prev_zero` init true — the
  mono→stereo transition semantics of `enc_API.c` 181–196, which fire at
  stream start since `nChannelsInternal` starts 0).
- Branches, in libopus order: panned mono (`width_prev_zero && (8·total <
  13·min_mid || frac·smth < 0.05)` → rates (total, 0), mid-only flag set,
  width 0); transition (same with `width_prev != 0`, thresholds 11·min_mid /
  0.02 → width 0, side still coded one frame); `smth > 0.95` → width 1;
  else width = smth. Tail: side ≥ 1 bps.
- Bitstream contract kept decoder-true: when mid-only is rate-driven (side
  trial not run), the header writes the side VAD as 0 and the mid-only icdf
  bit follows, matching libopus's forced `VAD_flags[1] = 0` (`enc_API.c` 456,
  460–462). `write_stereo_packet_header` now takes the two VAD bools;
  `prev_mid_only` still resets side-encoder state on the first side-coded
  frame after a mid-only run (`prev_decode_only_middle` analog).
- The old `side_energy < 1e-10` immediate mid-only and the
  `(bps/2).max(500)` side-rate reset in the `prev_mid_only` prologs are
  subsumed by the panned-mono rule and deleted. `set_bitrate` is now
  store-only; the split runs per frame in both `encode_frame_ms` and
  `encode_hybrid_ms` (mid energy is now accumulated alongside side energy).

## Named simplifications

1. `frac = sqrt(E_side/E_mid)` full-band — no LP/HP band split (libopus runs
   the ratio per band through the stereo predictors; ours are always zero).
2. Smoother coefficient fixed at 0.01/frame — no speech-activity scaling
   (our encoder does not track speech activity; test material is always
   active, where libopus's coefficient also lands near 0.01).
3. `silent_side_len` taper dropped — a no-op at 20 ms (320 − 128 = 192 ≥
   LA_SHAPE_MS·fs_kHz = 80); it only affects 10 ms frames, which this
   encoder's stereo paths never code.

## Measurements

Oracle rate table (`oracle_decodes_our_packets_across_the_rate_table`),
2 ch 16 kbps worst-channel corr: **.3407 → .4534** (was .4164 before r3).
Floor raised 0.33 → 0.44; r3 regression comment deleted.
Other 2 ch rows: 32 kbps .8943, 64 kbps .9773 — floors untouched, pass.
`silk_stereo_speech_roundtrip`: NB20 .9803/.9922, MB20 .9855/.9942,
WB20 .9906/.9956 (mono floors .9791/.9873/.9902/.9943; worst gap −.0018,
assert corr + 0.01 ≥ mono holds); NB40/WB60 identical per-channel corr.

Gate 2 (`silk_library_gate_vs_libopus`, mono, unchanged code path):
12k .8526, 16k .8646, 24k .9146, 32k .9391 — identical to r3 baseline;
RATE/LAG/DROPOUT gates pass.

## Variants measured and rejected

None — the libopus rule passed gate 1 on the first run (.4534 ≥ .40).

## Why 16 kbps lands at panned mono

16 kbps stereo WB: total = 15400, min_mid = 11600, frac ≈ .92 →
8·total = 123200 < 13·min_mid = 150800 → panned mono from frame 1,
permanent — exactly what libopus does at this rate; the mid keeps the full
15400 bps instead of starving between two channels.
