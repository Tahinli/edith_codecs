# lane-h265prune — a cheap SATD proxy to skip the CU split trial

## What was tried

`code_quadtree`'s split trial reruns the whole RD search one level down (up
to twice, at the two depths a quadtree reaches). Round 2 added a cheap
DC/planar-only SATD proxy (`CtuEncoder::quick_satd`) for the split's four
children, gated by a threshold `split_prune`: only estimate-competitive
splits pay for the real trial.

## Sweep table (round 2, BD-PSNR luma against x265, 1080p film + 2560x1440
screen capture, both real clips)

| threshold | film BD-PSNR | screen BD-PSNR | instructions (render_smooth profile) |
| --- | --- | --- | --- |
| off (0.0), baseline | +0.503 dB | +0.590 dB | 152.1G |
| 1.05 | +0.414 dB (Δ-0.089) | -4.770 dB (Δ-5.360) | 94.1G (-38%) |
| 1.15 | +0.437 dB (Δ-0.066) | -3.143 dB (Δ-3.733) | 100.1G (-34%) |
| 1.3 | +0.467 dB (Δ-0.036) | -1.973 dB (Δ-2.563) | 107.5G (-29%) |
| 1.5 | +0.481 dB (Δ-0.022) | -0.889 dB (Δ-1.479) | 118.1G (-22%) |
| 2.0 | +0.495 dB (Δ-0.008) | -0.068 dB (Δ-0.658) | 141.9G (-7%) |
| 3.0 | (flat, Δ~0) | (Δ-0.274) | (further shrinking) |

Deltas are against the unpruned `off` baseline on the same clip, which is the
number this lane's ~0.02 dB/class accept bar actually gates (the absolute BD
numbers already carry a pre-existing, unrelated gap to x265).

## Why it fails on its own (option 1 — global threshold)

Film tolerates aggressive pruning down to threshold ~1.5 before crossing
0.02 dB; screen capture is still 0.658 dB over the bar at threshold 2.0 and
0.274 dB over at 3.0. Fitting the observed decay (screen loss roughly ×0.42
per +1.0 threshold from 2.0→3.0) puts the 0.02 dB crossing near threshold
~6, where the pruning fire rate — and with it the instructions win — is
extrapolated under 5%, well below the ≥10% bar. No single global threshold
clears both classes with a real win; this is the content-opposite-optima
class again (film's flat-cost proxy vs screen's edge-dependent split value).

## Content-gate attempt (option 2)

Tried gating `split_prune` per picture on a cheap classifier — the p99/mean
ratio of the luma gradient histogram (peaky = flat backgrounds punctuated by
sharp text/UI edges = screen-like; smoother = film-like) — reusing the shape
of the existing chroma/luma gradient-ratio classifier
(`ec-h264/src/enc/mod.rs::chroma_qp_offset_for_source`), not its exact
metric (that one is tuned for chroma flatness, not film-vs-screen).

Sampled single frames from real film and real screen-capture library files
(`~/Videos/Films/Troy...`, `~/Videos/OBS/*`, `~/Videos/ab/obs-2026-08-17...`):

| source | frame ratio (p99/mean) |
| --- | --- |
| Troy film | 6.5, 7.4, 9.4, 10.0 |
| OBS screen capture (3 different recordings) | 10.1, 14.5, 17.3, 18.3, 21.6, 36.2, 36.2 |

Medians separate cleanly (~9 vs ~18) but the distributions overlap at the
edges (one screen-capture frame at 10.1 sits inside film's own range). A
per-picture classifier at any single threshold will misclassify some
screen-capture pictures as film-like and apply the aggressive proxy on
exactly the sharp-edge content it damages most — and this codec is
all-intra, one picture at a time, so there is no whole-clip average to fall
back on within a single `encode_idr_planes` call. Verifying a chosen
threshold's real BD impact would need the same real-clip x265 sweep as
option 1, repeated with gating live, which this round's budget did not
cover; shipping an unverified gate on a class-quality lever was rejected
rather than guessed.

## Decision: REJECTED

Both the flat global threshold and the content-gated threshold fail the
charter's bar (option 1 provably, option 2 for lack of a safe, verified
threshold). Per the charter's option 3, the depth-pruning machinery
(`quick_satd`, `split_prune`, the widened CTB-32-vs-64 tolerance, the
re-pinned RDOQ cache hashes) is reverted. The lane closes with round 1's
merged all-zero-CBF early-out (`adca476`, already on `main`) as its only
ship.

## Gates

- `cargo test -p ec-h265 --release`: see suite log referenced in the commit;
  the tree is bit-for-bit round 1's state (`adca476`), so this is a
  reconfirmation, not a new result.

## Not in this lane

- A whole-clip (not per-picture) content classifier, which would need a
  first pass over the source before the per-picture encode loop starts —
  architecture change out of scope here.
- A directional (not DC/planar-only) split proxy, which round 2's own doc
  comment named as the follow-up lever for screen content; still unbuilt.
