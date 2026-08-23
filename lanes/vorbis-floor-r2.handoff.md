# lane-vorbis-floor round 2 — handoff

## Verdict
REFUTED. Peak is the best per-region floor metric. Envelope-following (geomean, geomean+k dB, RMS) all regress monotonically as the floor drops below the region peak. encode.rs reverted to main (7c3f1e9). Suite green.

## Final state
- encode.rs: at main, unmodified. `git diff 7c3f1e9 -- crates/ec-vorbis/src/encode.rs` = empty.
- 5 commits on lane-vorbis-floor past main: E1, E2 k=12, E2 k=6, E3 RMS, revert.
- Suite: `cargo test -p ec-vorbis` = 13 lib + 8 oracle, all green.
- Report: `lanes/vorbis-floor-r2.report.md` (verdict line first).
- No merge, no push.

## Experiments summary

| config                    | mean gap | vs baseline |
|---------------------------|----------|-------------|
| baseline (peak)           | 0.01525  | —           |
| E3 RMS                    | 0.07285  | 4.8× worse  |
| E2 geomean + 12 dB        | 0.13535  | 8.9× worse  |
| E2 geomean + 6 dB         | 0.16564  | 10.9× worse |
| E1 geomean (k=0)          | 0.183    | 12.0× worse |

Monotonic: geomean → k6 → k12 → RMS → peak, strictly improving.

## Key findings
1. **Peak is optimal for 12-point grid**: each region spans ~73 bins; peak-to-mean ratio is 20–30 dB. Envelope metrics collapse toward the noise floor, and the 8-pass clamp-lift refit cannot recover.
2. **Clamp-lift saturation is rare**: 10/509 clamped blocks (2%) at baseline. naz and dl8a never clamp. E4 unnecessary.
3. **Architectural gap**: libvorbis's 65-point floor1 makes envelope≈peak per region. Our 12-point grid (r1 proved more points = worse) makes them diverge. The gap is structural, not metric-level.

## What to try next (not this lane)
- Per-band bit allocation (Bark-band masking model vs global headroom scalar).
- Floor1 curve shape (multiplicative Y values, per-point amplitude).
- Residue book selection (multiple books per partition vs single-book-per-class).
