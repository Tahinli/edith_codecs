verdict: REFUTED — long-floor post count/spacing is not the lever; gap worsens monotonically with more posts.

# lane-vorbis-floor round 1 — floor geometry at equal size

Gate: `real_library_sweep_vs_reference`, SWEEP_ONLY=her,dl8a,nik,naz (8 rows), ours at
libvorbis's measured kbps, both decoded by our decoder. Mean corr gap over the 8 rows:

| config                          | mean gap |
|---------------------------------|----------|
| baseline FLOOR_POINTS_LONG=12   | 0.01525  |
| E1 = 16 (log spacing)           | 0.01786  |
| E1 = 24                         | 0.02171  |
| E1 = 32                         | 0.02594  |
| E2 = 12, Bark-equal spacing     | 0.02163  |
| E3 short 8→12                   | skipped (nothing beat baseline) |

Bit split (baseline vs E2, `vorbis-floor-r1.*.bits.txt`): bits per non-zero unchanged
(nik 96k 5.42 → 5.47, her 96k 6.60 → 6.71) and non-zero count unchanged (2.56M → 2.52M).
Moving posts into 300–1080 Hz left that region's dNSR at +9..+12 dB (her 96k bands table).

Reading: residue density is set by the quantiser step, not the post grid. `peaks_of`
peak-holds each post region and `fit_floor` puts the floor at peak − headroom, so the
step inside a region follows its loudest bin: that bin is coded finely (large q, ~6 bits)
and bins 20–40 dB under it quantise to zero, whatever the region width. libvorbis's floor
tracks the envelope, so its step is ~local level/3 and it codes twice the non-zeros at
~3 bits. Next: envelope-following floor (region mean-dB/RMS instead of max), rate loop
rebalancing headroom, clamp-lift catching peaks past the widest book.
