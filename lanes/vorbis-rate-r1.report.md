# lane-vorbis-rate r1 — ≥100 kbps target scale 1.0→0.98 (verdict: 14/14 PASS)

VERDICT: 14-row equal-size sweep 14/14 PASS (r3 start 13/14, dl8a@128 FAIL). Only failure fixed: dl8a@128 rate +3.50%→+1.41%. No row's gap worse than baseline by >.001 (max delta +.0003). Worst |rate| within ±3% gate: her@96 −2.94% (pre-existing, <100 k scale untouched), sadie@128 +2.48%. 0 dropout seconds all rows. Suite green.

## Change (one lever, rung 6 — minimum code that works)
- `encode.rs`: split RATE_TARGET_SCALE into 0.97 (<100 k, unchanged) and new `RATE_TARGET_SCALE_HI = 0.98` (≥100 k, was 1.0). Two-line edit in `update_rate` selects the high-rate const.
- Root cause: at scale 1.0 every ≥100 k row realised ~2-3% hot (rate ≈ proportional to target scale in steady state); a 2% target trim centres them. The <100 k rows are mixed-sign (sadie/hein@96 already +2.05..+2.74) so bumping 0.97→0.985 there would push those two over +3% — NOT applied (lever 1 "and/or" → only the ≥100 k half).

## Measured (full sweep, lanes/vorbis-rate-r1.full.txt)
≥100 k rows before→after (rate%): dl8a@128 +3.50→+1.41, naz@128 +2.98→+0.88, zaur@128 +2.80→+0.68, nik@128 +2.48→+0.41, sadie@128 +2.48→+2.48*, hein@128 +1.78→+1.78*, her@128 +0.41→−1.45. (* sadie/hein@128 realise ~91-92 kbps from a 128 k target — content-driven, the scale trims the target not the floor, and their realised rate barely moves because they already spend below target; rate% vs ref unchanged.)

## Gap deltas vs r3 baseline (all ≤ +.0003, limit +.001)
nik@128 .0020→.0023, zaur@128 .0004→.0007, her@128 .0008→.0010, naz@128 −.0003→−.0002, dl8a@128 .0014→.0016. All <100 k rows unchanged (scale untouched).

## Refuted / not tried
- <100 k scale 0.97→0.985: rejected pre-run by arithmetic (sadie@96 +2.74→~+4.2, hein@96 +2.05→~+3.5 → FAIL).
- Continuous linear scale 0.975@80k→0.98@125k (lever 2): not needed — the step at 100 k already lands every row within ±2.94% and no gap grew >.001.
- Reservoir repay 25%→35% at ≥100 k (lever 3): not needed — rate already on target.

## Deferred
none.
