# lane-vorbis-floor r3 — headroom tilt (verdict: PARTIAL PASS, merged)

VERDICT: equal-size sweep 11/14 PASS (baseline 2/14), mean gap .0153 → .0031. Residual: sadie@96 (+7.1% rate, gap .0093), sadie@128 (gap .0060), hein@96 (+4.3% rate, gap .0086).

## Lever
`headroom_tilt(hz)` in `crates/ec-vorbis/src/encode.rs`: per-post headroom offset added to `fit_floor` target.
LF +12 dB ≤1.1 kHz tapering to 0 at 2 kHz; HF −3 dB from 4 kHz tapering to full at 6 kHz; long blocks only.
Rationale (lanes/vorbis-psy-r1.histogram.txt): libvorbis codes LF with finer step (more large q), HF dense ±1 where we zeroed — one global headroom was frequency-blind.

## Refuted on this lane
- E3 RMS-based peaks in place of peak-hold: mean gap .0073 (worse than tilt .0019 on 8 rows) — reverted.
- E4a HF taper 6→8 kHz: no gain over 4→6 kHz.
- r1 post-grid density, r2 envelope floor: see vorbis-floor-r1/r2 reports.

## Full 14-row sweep (ours at libvorbis's measured kbps, `real_library_sweep_vs_reference`)
source  kbps   ours_kbps ref_kbps rate%   corr_ours corr_ref gap     verdict
nik     96     83.5      84.5      -1.20 0.9863    0.9903   0.0040  PASS
nik     128    115.0     115.8     -0.64 0.9927    0.9951   0.0024  PASS
zaur    96     80.7      81.8      -1.29 0.9899    0.9922   0.0023  PASS
zaur    128    108.1     108.6     -0.47 0.9947    0.9955   0.0009  PASS
her     96     88.1      89.9      -1.96 0.9879    0.9867   -0.0012 PASS
her     128    121.8     124.9     -2.49 0.9917    0.9927   0.0011  PASS
naz     96     84.6      85.5      -1.08 0.9932    0.9923   -0.0009 PASS
naz     128    120.6     120.9     -0.18 0.9969    0.9968   -0.0001 PASS
sadie   96     84.9      79.3      +7.07 0.9772    0.9865   0.0093  FAIL
sadie   128    93.1      91.0      +2.31 0.9846    0.9906   0.0060  FAIL
dl8a    96     86.1      86.9      -0.96 0.9878    0.9916   0.0038  PASS
dl8a    128    117.6     117.8     -0.12 0.9938    0.9957   0.0019  PASS
hein    96     82.4      79.1      +4.26 0.9784    0.9869   0.0086  FAIL
hein    128    91.1      90.7      +0.51 0.9856    0.9906   0.0050  PASS

## Residual class
sadie/hein are the HF-droop files (15.5–24 kHz −5..−19 dB, psy-r1 bands). At 96 k the rate loop overshoots the 79 kbps reference by 4–7% — floor cost dominates at low rate and the tilt raises LF floor resolution cost. Next lever is HF-specific (co-mask cap / point-stereo cutoff), not headroom.

## Suite
`cargo test -p ec-vorbis`: 13+8 passed, 3 ignored (sweeps).
