verdict: REFUTED — peak is the best per-region floor metric; any deviation below the region peak worsens quality monotonically. encode.rs left at main.

# lane-vorbis-floor round 2 — envelope-following floor

Gate: `real_library_sweep_vs_reference`, SWEEP_ONLY=her,dl8a,nik,naz (8 rows), ours at
libvorbis's measured kbps, both decoded by our decoder. Mean corr gap over the 8 rows:

| config                              | mean gap | corr range   | rate %      |
|-------------------------------------|----------|--------------|-------------|
| **baseline (peak)**                 | **0.01525** | 0.97–0.99 | ±1.1%       |
| E3 = per-region RMS                 | 0.07285  | 0.82–0.97    | −1.2 to −2.0% |
| E2 = geomean + 12 dB (k=12)         | 0.13535  | 0.78–0.95    | −0.9 to +2.7% |
| E2 = geomean + 6 dB (k=6)           | 0.16564  | 0.73–0.91    | −0.7 to −4.0% |
| E1 = geomean (k=0)                  | 0.183    | 0.71–0.88    | up to −10.6% |

Monotonic trend: geomean(0.183) → k6(0.166) → k12(0.135) → RMS(0.073) → peak(0.015).
Every step toward the peak improves the gap. The peak is the optimal endpoint; no
envelope metric beats it.

## Per-track detail (baseline)

| track | kbps  | ours_kbps | ref_kbps | rate%  | corr_ours | corr_ref | gap     |
|-------|-------|-----------|----------|--------|-----------|----------|---------|
| nik   | 96    | 84.0      | 84.5     | −0.61  | 0.9690    | 0.9903   | 0.0213  |
| nik   | 128   | 115.5     | 115.8    | −0.24  | 0.9838    | 0.9951   | 0.0113  |
| her   | 96    | 89.9      | 89.9     | +0.04  | 0.9668    | 0.9867   | 0.0199  |
| her   | 128   | 123.5     | 124.9    | −1.13  | 0.9810    | 0.9927   | 0.0117  |
| naz   | 96    | 85.4      | 85.5     | −0.12  | 0.9792    | 0.9923   | 0.0131  |
| naz   | 128   | 121.3     | 120.9    | +0.39  | 0.9914    | 0.9968   | 0.0054  |
| dl8a  | 96    | 86.9      | 86.9     | −0.01  | 0.9655    | 0.9916   | 0.0261  |
| dl8a  | 128   | 118.7     | 117.8    | +0.80  | 0.9826    | 0.9957   | 0.0132  |

## Root cause analysis

### Why envelope-following fails here

Each long-block floor point owns ~73 bins (1024 / 14 regions). In a typical audio
spectrum, most of those 73 bins sit at the noise floor (−60 to −80 dB) while only a
few carry signal (−20 to −40 dB). The envelope metrics collapse toward the noise floor:

- **Geometric mean** is dominated by the 60+ noise-floor bins: geomean_dB ≈ −59 dB.
  Even +12 dB margin leaves the floor at −47 dB, 27 dB below the signal peak.
  The clamp-lift refit (max 8 passes × 6 dB = 48 dB) saturates trying to recover.
- **RMS** (sqrt(mean(bin²))) weights loud bins quadratically, so it sits at ~−40 dB,
  closer to the peak. Better than geomean but still 20 dB below the signal peak.
- **Peak** captures the loudest bin directly. The floor sits at peak − 9 dB
  (MASKING_OFFSET_DB), so the quantiser step is ~3× the peak — every bin within
  9 dB of the peak codes as a small residue, and bins further down quantise to zero.

The clamp-lift refit (encode_block :832) can raise the floor by at most 8 × 6 dB = 48 dB
across 8 passes. With a geomean-based floor 27 dB below the signal, the refit must
recover 27 dB in lifts — but the `excess` factor (1.05× the over-limit ratio) is
multiplicative, not additive, so convergence is slow and the 8-pass cap is hit before
the floor reaches the signal level. The peak-based floor needs at most 1–2 lifts for
the rare bin that exceeds the residue book range (±127), so it rarely saturates.

### Clamp-lift saturation at baseline

Instrumented with static atomic counters (VORBIS_FLOOR_DEBUG=1), 8-row gate:

| track | kbps | clamped blocks | saturated (8-pass) | sat % of clamped |
|-------|------|----------------|--------------------|--------------------|
| nik   | 96   | 17             | 1                  | 5.9%               |
| nik   | 128  | 58             | 2                  | 3.4%               |
| her   | 96   | 194            | 4                  | 2.1%               |
| her   | 128  | 240            | 3                  | 1.3%               |
| naz   | 96   | 0              | 0                  | —                  |
| naz   | 128  | 0              | 0                  | —                  |
| dl8a  | 96   | 0              | 0                  | —                  |
| dl8a  | 128  | 0              | 0                  | —                  |

Total: 10 saturated out of 509 clamped blocks (2.0%). naz and dl8a never clamp at all.
Clamping is not the dominant quality bottleneck at baseline — E4 (capping the floor
drop at peak − 36 dB) would address a 2% edge case and cannot close a 4.8× gap.

### Why libvorbis's envelope works but ours doesn't

libvorbis uses a finer floor1 curve (up to 65 X points in a long block, vs our 12).
Each point owns ~16 bins, so the peak-to-mean ratio within a region is 6–10 dB, not
20–30 dB. At that resolution, the envelope and the peak nearly coincide, and the
floor follows the local level smoothly. Our 12-point grid forces each region to span
73 bins, where the peak and the envelope diverge by 20+ dB. The peak is the only
metric that keeps the floor close enough to the signal for the quantiser to work.

The r1 report already refuted adding more posts (gap worsens monotonically: 12→16→24→32).
So the coarse grid is both necessary (more posts = worse) and incompatible with
envelope-following (73-bin regions have too much peak-to-mean spread). The quality
gap is architectural: it comes from the floor1 post count and the per-band bit
allocation strategy, not from the per-region level metric.

## What was tried

1. **E1** (geomean, k=0): `peaks_of` returns geometric mean of |bin| per region.
   Gap 0.183, rate undershot up to −10.6%. Severe regression.
2. **E2 k=12**: geomean + 12 dB margin. Gap 0.135. Still 8.9× baseline.
3. **E2 k=6**: geomean + 6 dB margin. Gap 0.166. Worse than k=12 — confirms monotonicity.
4. **E3** (RMS): `peaks_of` returns sqrt(mean(bin²)) per region. Gap 0.073. Best
   envelope metric but still 4.8× baseline.
5. **Clamp-lift saturation count**: instrumented the refit loop. 2% saturation at
   baseline. Clamping is not the bottleneck; E4 unnecessary.
6. **Reverted encode.rs to main** (7c3f1e9). Suite green (13 lib + 8 oracle).

## Next direction

The floor-level metric is exhausted. The gap is structural: our 12-point floor1 grid
with peak-hold + 9 dB headroom is the best per-region metric for this resolution, and
the resolution itself cannot be increased (r1 refuted that). The remaining gap to
libvorbis likely comes from:

- **Per-band bit allocation**: libvorbis allocates bits per Bark band with masking
  models; our rate loop adjusts a global headroom scalar. A per-band allocation could
  spend bits where the floor is highest and save them where it's low.
- **Floor1 curve shape**: libvorbis uses multiplicative floor1 Y values with
  per-point amplitude, not a linear-interpolated peak-hold. The interpolation between
  our 12 peak-held points may overshoot or undershoot between posts.
- **Residue book selection**: libvorbis chooses from multiple residue books per
  partition; our single-book-per-class approach may be suboptimal for mixed content.
