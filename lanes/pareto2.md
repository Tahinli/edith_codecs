# lane-pareto2 -- working notes

Head: f6c67770 (+ this lane). Box: 12 cores, TWO other lanes running throughout,
load 9-25 (load 64 once, during the first fps sweep). Every arm is
`encode::tests::bd_rate_screen_native` (12 frames, gop 12, four quantizers,
1920x768 / 1920x1024 native crops, 1 tile, 1 thread), logs in
`$HOME/.cache/pareto2/logs/`.

RULE USED FOR WALL: BD is exact across runs (the same stream), wall is not.
Phase-1 arms ran THREE at a time, so their wall is read only as the
ours:rav1e-speed-6 ratio INSIDE one arm; the lever arms that decided a shipped
row were re-run ONE at a time (`q5.tsv`).

## Phase 1 -- the Pareto table, our presets (q1.tsv, 15 arms, all RC=0)

BD-rate vs libaom `cpu-used 6` / vs rav1e `speed 6`; fps = 48 coded frames over
our own ladder wall in that arm, `ratio` = ours:rav1e-s6 wall in the same arm.

| preset | film A | film B | screen | ratio A/B/screen | 1-thread fps A/B/screen |
|---|---|---|---|---|---|
| 0 | +21.7 / -4.4 | +26.9 / -0.6 | +19.8 / -30.5 | 10.9 / 9.5 / 7.5 | 0.24 / 0.28 / 0.37 |
| 3 | +22.3 / -4.1 | +27.3 / -0.3 | +23.7 / -28.4 | 7.4 / 6.6 / 4.4 | 0.40 / 0.48 / 0.60 |
| 6 | +26.6 / -0.4 | +34.7 / +6.4 | +23.1 / -28.5 | 2.2 / 3.9 / 3.0 | 0.71 / 0.47 / 0.71 |
| 8 | +61.1 / +23.5 | +75.6 / +38.4 | +50.8 / -13.8 | 1.0 / 1.5 / 1.3 | 1.50 / 1.34 / 2.10 |
| 10 | +112.0 / +64.4 | +155.0 / +102.7 | +106.0 / +12.7 | 0.8 / 1.4 / 0.6 | 2.46 / 2.03 / 4.17 |

## Phase 1b -- the references at THEIR fast presets, same arms (EC_AV1_PARETO_REFS=1)

| row | encoder | vs libaom cpu-6 | vs rav1e s6 | ladder wall (rav1e s6 anchor in that arm) |
|---|---|---|---|---|
| film A | ours preset 6 | +26.6 | -0.4 | 67.6s (31.3s) |
| film A | rav1e speed 8 | +29.0 | +2.0 | 24.1s |
| film A | rav1e speed 10 | +45.3 | +17.1 | 8.4s |
| film A | libaom cpu-used 8 | +0.0 | -20.1 | 15.4s |
| film A | SVT-AV1 preset 8 | +24.1 | -1.9 | 2.4s |
| film A | SVT-AV1 preset 10 | +45.3 | +12.1 | 2.0s |
| film B | ours preset 6 | +34.7 | +6.4 | 103.0s (26.4s) |
| film B | rav1e speed 8 | +37.3 | +3.5 | 20.5s |
| film B | rav1e speed 10 | +61.4 | +28.7 | 10.5s |
| film B | libaom cpu-used 8 | +0.0 | -22.2 | 32.3s |
| film B | SVT-AV1 preset 8 | +41.7 | +7.4 | 2.9s |
| film B | SVT-AV1 preset 10 | +65.9 | +23.3 | 2.0s |
| screen | ours preset 6 | +23.1 | -28.5 | 68.0s (22.6s) |
| screen | rav1e speed 8 | +84.2 | +4.5 | 13.8s |
| screen | rav1e speed 10 | +332.5 | +103.4 | 9.8s |
| screen | libaom cpu-used 8 | +0.0 | -42.4 | 17.6s |
| screen | SVT-AV1 preset 8 | +21.5 | -31.6 | 5.0s |
| screen | SVT-AV1 preset 10 | +39.4 | -24.5 | 1.9s |

`libaom cpu-used 8` reads +0.0% against `cpu-used 6` on every row: this ffmpeg's
libaom codes the SAME bytes at both (the old `lanes/pareto.md` run saw the same
thing), so it is one point on the plot, not two.

NOTE (open): one SVT-AV1 preset-8 screen point decodes one sample off through
OUR decoder (`frame 0 plane Y sample 1471489: ours 53, ffmpeg 52`). Reference
rows collect that as a note, not a gate failure; it is a real decoder finding.

## Phase 2 -- per-lever re-price at preset 6, film A (q2.tsv, 21 arms, all RC=0)

Control +26.6 / -0.4. "gain" = BD points the lever takes off the vs-rav1e column.

| lever re-enabled at 6 | vs libaom / rav1e | gain |
|---|---|---|
| extra-reference NEWMV | +24.4 / -2.4 | 2.0 |
| coefficient breakout 0 | +24.8 / -1.6 | 1.2 |
| 8x8 split | +25.9 / -0.8 | 0.4 |
| warp | +26.2 / -0.8 | 0.4 |
| CfL | +26.2 / -0.7 | 0.3 |
| chroma top-k off | +26.3 / -0.7 | 0.3 |
| inter var-tx | +26.3 / -0.7 | 0.3 |
| angle delta | +26.5 / -0.6 | 0.2 |
| split-RD 0.125 | +26.4 / -0.6 | 0.2 |
| inter-intra top-13 | +26.7 / -0.3 | 0.1 |
| intra top-13 | +26.7 / -0.4 | 0.0 |
| filter intra | +26.7 / -0.4 | 0.0 |
| 32x32 tx depth | +26.6 / -0.4 | 0.0 (inert: inter tx select is off at 6) |
| compound var-tx | +26.6 / -0.4 | 0.0 (same cause) |
| loop restoration | +26.7 / -0.2 | -0.2 (worse) |

| lever preset 6 KEEPS, disabled | vs libaom / rav1e | cost of losing it |
|---|---|---|
| RDOQ | +41.2 / +10.2 | 10.6 |
| 64x64 inter root | +30.7 / +3.4 | 3.8 |
| key 64x64 intra root | +28.0 / +0.9 | 1.3 |
| tpl window 4 -> 1 | +27.0 / -0.3 | 0.1 |
| per-SB delta_q | +27.2 / -0.2 | 0.2 |

## Phase 3 -- wall of the two candidates, serial arms

See `q3.tsv`/`q5.tsv` logs; the threaded fps sweep (`fps2.log`) is NOT usable at
this box load -- preset 3 read slower than preset 0 in every pass.
