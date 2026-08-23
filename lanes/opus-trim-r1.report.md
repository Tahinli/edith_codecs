# lane opus-trim r1 — libopus 1.3+ alloc_trim_analysis port: REJECTED

## What was tried (diff kept at scratchpad lanes/opus-trim-r1.diff, not merged)
- `alloc_trim_analysis`: base 4 below 64 kb/s ramping to 5 at 80 kb/s; continuous stereo term `0.75·log2(1.001−xc²)` (floor −4); continuous tilt `(diff+1)/6` clamped ±2; `−2·tf_estimate`; `floor(.5+trim)`; `stereo_saving` state.
- `compute_vbr` stereo_saving term (`target −= min(max_frac·target, (saving−0.1)·dof·8)`).
- hybrid (`start > 0`) keeps trim 5, stereo_saving 0.

## Gate (14 rows vs lanes/opus-int-r1.sweep.txt) — FAIL
```
nik@64k: gap=+0.0001,   prev=-0.0020
nik@96k: gap=-0.0001,   prev=-0.0006
zaur@64k: gap=+0.0008,  prev=-0.0018
zaur@96k: gap=-0.0003,  prev=-0.0008
her@64k: gap=-0.0007,   prev=-0.0031
her@96k: gap=-0.0004,   prev=-0.0008
naz@64k: gap=-0.0007,   prev=-0.0028
naz@96k: gap=-0.0003,   prev=-0.0008
sadie@64k: gap=+0.0047  prev=+0.0038
sadie@96k: gap=-0.0001  prev=-0.0000
dl8a@64k: gap=+0.0014,  prev=-0.0017
dl8a@96k: gap=+0.0002,  prev=-0.0006
hein@64k: gap=+0.0035,  prev=+0.0022
hein@96k: gap=-0.0001,  prev=-0.0002
RATE GATE: all rows within ±5%
DROPOUT GATE: passed (no ours-only dropouts)
```
Rate unchanged (±0.1 kbps per row) — the reservoir clamp, not the target, sets the rate; the loss is the trim itself. Suite was green (34+27).

## Class
Third libopus term rejected on top of our allocation (tf_estimate trim/VBR terms ×2, now the full trim analysis): libopus pieces do not compose onto a partially-ported VBR target (our `7/4` transient stand-in, `0.012·target` tf stand-in, no tonality). Next trim attempt must port `compute_vbr` whole first, then measure the trim on top.
