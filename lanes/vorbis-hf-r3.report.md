# lane-vorbis-hf r3 — rate-loop windup + HF co-mask cap (verdict: 13/14 PASS at equal size)

VERDICT: 14-row equal-size sweep 13/14 PASS (r3 start 11/14, tilt baseline 2/14); max corr gap +.0027 (nik@96), 7 rows ours > libvorbis; 0 dropout seconds; only failure dl8a@128 rate +3.50% (gap .0014, limit ±3%).

## Levers kept (each measured on SWEEP_ONLY=sadie,hein then 14 rows; see lanes/vorbis-hf-r2.handoff.md / r3 log)
- revert r1's 0.93 target hack at <96k (regressor).
- reservoir_debt clamped to ±8× per-block budget after accrual — windup was 831 k bits → 91 s of headroom suppression → post-transient dropouts (class: rate-reservoir windup, same as ec-opus 7172437).
- HEADROOM_SHORT_MIN 31 dB + ramp for short blocks.
- RATE_TARGET_SCALE split at 100 kbps (0.97 below, 1.0 at/above): rows realising below ref needed the cut, high-rate rows undershot with it.
- HF co-mask cap: bark > 19 uses co_mask − 30 dB only below 100 kbps (r1 lever; at ≥100 k it cost nik/zaur/her@128 +.001 gap).

## Refuted with numbers (builder log)
- global RATE_TARGET_SCALE 1.0: sadie@96 rate overshoot.
- flat HEADROOM_SHORT_MIN for all sub-128k: high-rate gaps worse.
- scale split keyed at 128k: her@128 ref realises 124.9k, below the key.

## Residual
dl8a@128: ours 121.9 vs ref 117.8 kbps (+3.50%), corr gap .0014. Rate-side only; the scale split at 100 k over-spends on this one high-rate row. Debt.

## Full table (lanes/vorbis-hf-r3.full.txt)
nik     96     83.2      84.5      -1.60 0.9876    0.9903   0.0027  0.967    0.976   0     PASS
nik     128    118.6     115.8     +2.48 0.9932    0.9951   0.0020  0.978    0.984   0     PASS
zaur    96     80.6      81.8      -1.43 0.9910    0.9922   0.0012  0.960    0.975   0     PASS
zaur    128    111.7     108.6     +2.80 0.9951    0.9955   0.0004  0.975    0.983   0     PASS
her     96     87.2      89.9      -2.94 0.9873    0.9867   -0.0006 0.950    0.947   0     PASS
her     128    125.4     124.9     +0.41 0.9919    0.9927   0.0008  0.963    0.969   0     PASS
naz     96     84.2      85.5      -1.52 0.9941    0.9923   -0.0018 0.959    0.966   0     PASS
naz     128    124.4     120.9     +2.98 0.9971    0.9968   -0.0003 0.974    0.981   0     PASS
sadie   96     81.4      79.3      +2.74 0.9876    0.9865   -0.0010 0.909    0.927   0     PASS
sadie   128    93.3      91.0      +2.48 0.9919    0.9906   -0.0012 0.935    0.950   0     PASS
dl8a    96     86.4      86.9      -0.64 0.9901    0.9916   0.0015  0.972    0.982   0     PASS
dl8a    128    121.9     117.8     +3.50 0.9944    0.9957   0.0014  0.982    0.989   0     FAIL
hein    96     80.7      79.1      +2.05 0.9888    0.9869   -0.0018 0.911    0.937   0     PASS
hein    128    92.3      90.7      +1.78 0.9925    0.9906   -0.0019 0.941    0.949   0     PASS
source  kbps   ours_kbps ref_kbps rate%   corr_ours corr_ref gap     minsec_o minsec_r drops verdict
