# lane-tplwin — the tpl lookahead window per speed preset

Base `45a6f786`. Scope: `speed::TPL_DEPTH` (the per-preset lookahead window).

## Why the lane exists

`speed.rs:438` shipped `TPL_DEPTH = [8, 1, 1, ..., 1]`: the temporal lambda map
is inert at every preset above 0, and lane-deltaq's per-SB `delta_q` (which
rides that map) is inert with it. Since RDOQ landed the fast presets are what
a user without hardware runs, so the map's price at those presets is the
question.

## Method

`bd_rate_screen_native --ignored`, 12 frames, four quantizers, all five rows,
one arm per (preset, depth) through `EC_AV1_SPEED` / `EC_AV1_TPL_D`
(`lanes/tw-arm.sh`, logs `lanes/tw-<arm>.log`). Two arms at a time on a box
also running another lane (load average 13-15 throughout), so WALL IS READ AS
THE RATIO ours:rav1e INSIDE ONE ARM, never across arms (class: wall tables are
only comparable inside one interleaved batch). Best-of-2 was not affordable at
this load and is not claimed.

Note: the gate's film rows code through `encoder::Av1Encoder::drain_pending`,
whose window is the mini-GOP's display successors, so depth is capped by the
group at 8 anyway; the screen row is content-gated flat and takes
`encode_sequence_with_ctx`'s plain successor window.

## Results

Every cell is BD-rate vs libaom `cpu-used 6` / vs rav1e `speed 6` (lower is
better). `wall` is ours:rav1e inside the arm. `bars` rows are `testsrc2`
colour-bar fixtures, recorded, never a decision.

### Preset 3 (batch: the two arms ran side by side, load ~14)

| row | depth 1 (control) | depth 8 | delta |
|---|---|---|---|
| bars 1080p | +43.9 / +21.2 | +44.2 / +21.4 | +0.3 / +0.2 |
| bars 2160p | +49.1 / +18.9 | +48.9 / +18.9 | -0.2 / 0.0 |
| **film A** | **+22.5 / -3.9** | **+22.5 / -3.9** | **0.0 / 0.0** |
| **film B** | **+27.6 / +0.1** | **+27.7 / +0.1** | **+0.1 / 0.0** |
| **screen** | **+33.5 / -23.1** | **+32.9 / -23.4** | **-0.6 / -0.3** |
| wall film A | 165.4s:23.8s = 6.95 | 165.9s:24.8s = 6.69 | -3.7% |
| wall film B | 142.5s:26.2s = 5.44 | 139.1s:26.1s = 5.33 | -2.0% |
| wall screen | 76.9s:13.4s = 5.74 | 77.1s:13.7s = 5.63 | -1.9% |

At preset 3 the lookahead pass does not show up in the wall at all -- the
ratio moves 2-4% the WRONG way, i.e. the pass is smaller than this box's
noise -- and no film row moves. Logs `lanes/tw-p3d{1,8}.log`.

### Preset 6 (batch: the two arms ran side by side, load ~14)

| row | depth 1 (control) | depth 8 | delta |
|---|---|---|---|
| bars 1080p | +67.6 / +39.5 | +69.2 / +40.7 | +1.6 / +1.2 |
| bars 2160p | +63.8 / +29.0 | +64.1 / +29.4 | +0.3 / +0.4 |
| **film A** | **+27.0 / -0.3** | **+27.0 / -0.4** | **0.0 / -0.1** |
| **film B** | **+35.9 / +7.2** | **+35.6 / +6.9** | **-0.3 / -0.3** |
| **screen** | **+34.7 / -22.4** | **+34.9 / -22.3** | **+0.2 / +0.1** |
| wall film A | 75.3s:20.7s = 3.64 | 77.1s:19.5s = 3.95 | +8.5% |
| wall film B | 62.6s:17.9s = 3.50 | 63.0s:17.5s = 3.60 | +2.9% |
| wall screen | 37.6s:12.4s = 3.03 | 39.0s:12.1s = 3.22 | +6.3% |

Preset 6's search is 2x cheaper than preset 3's, so the SAME lookahead pass is
+3..8% of the wall here. Logs `lanes/tw-p6d{1,8}.log`.

### Preset 0 -- is 8 still the optimum under the pyramid window? (batch: two arms side by side)

| row | depth 8 (shipped) | depth 4 | delta of 4 |
|---|---|---|---|
| bars 1080p | -1.0 / -16.8 | -1.4 / -17.1 | -0.4 / -0.3 |
| bars 2160p | +9.4 / -12.7 | +9.9 / -12.2 | +0.5 / +0.5 |
| **film A** | **+21.7 / -4.4** | **+21.5 / -4.5** | **-0.2 / -0.1** |
| **film B** | **+26.9 / -0.6** | **+27.6 / +0.1** | **+0.7 / +0.7** |
| **screen** | **+27.8 / -26.0** | **+27.7 / -26.1** | **-0.1 / -0.1** |
| wall film A | 224.0s:21.1s = 10.62 | 226.3s:20.5s = 11.04 | +4.0% |
| wall film B | 157.9s:18.8s = 8.40 | 149.6s:19.3s = 7.75 | -7.7% |
| wall screen | 76.6s:12.4s = 6.18 | 77.4s:12.7s = 6.09 | -1.4% |

Film B decides it: shortening the window to 4 costs 0.7 points on BOTH columns
of the 2160p film, past the keep rule's bound, for 7.7% of that row's wall,
while film A moves 0.2 the other way and screen 0.1. **8 stays the preset-0
default, so the byte pins do not move.** Logs `lanes/tw-p0d{8,4}.log`.

### Depth 4 (the arm that ships at presets 3..6)

| row | preset 3 d1 | preset 3 **d4** | preset 3 d8 | preset 6 d1 | preset 6 **d4** | preset 6 d8 |
|---|---|---|---|---|---|---|
| bars 1080p | +43.9 / +21.2 | +43.1 / +20.4 | +44.2 / +21.4 | +67.6 / +39.5 | +68.4 / +40.0 | +69.2 / +40.7 |
| bars 2160p | +49.1 / +18.9 | +49.2 / +19.0 | +48.9 / +18.9 | +63.8 / +29.0 | +62.7 / +28.0 | +64.1 / +29.4 |
| film A | +22.5 / -3.9 | +22.3 / -4.1 | +22.5 / -3.9 | +27.0 / -0.3 | +27.2 / -0.2 | +27.0 / -0.4 |
| film B | +27.6 / +0.1 | +27.3 / -0.3 | +27.7 / +0.1 | +35.9 / +7.2 | +35.2 / +6.6 | +35.6 / +6.9 |
| screen | +33.5 / -23.1 | +33.4 / -23.2 | +32.9 / -23.4 | +34.7 / -22.4 | +34.8 / -22.3 | +34.9 / -22.3 |

Logs `lanes/tw-p{3,6}d4.log`. The preset-6 depth-1 and depth-4 arms were then
re-run SIDE BY SIDE (`lanes/tw-p6d{1,4}b.log`) both to price depth 4's wall
in-batch and to check the box: every ladder point came back byte-identical to
the first run, so nothing in the BD columns above is run noise.

| preset 6, in-batch | depth 1 | depth 4 | ratio delta |
|---|---|---|---|
| film A wall | 64.6s:15.4s = 4.19 | 66.0s:16.0s = 4.13 | -1.6% |
| film B wall | 54.2s:16.4s = 3.30 | 53.6s:17.6s = 3.05 | -7.8% |
| screen wall | 39.2s:12.1s = 3.24 | 39.4s:11.9s = 3.31 | +2.2% |

Depth 4's cost straddles zero on a box at load 13-15; depth 8's, measured the
same way, was +3/+2.9/+6.3% -- all three positive, which is the one wall
statement this batch supports.

## Shipped

    speed::TPL_DEPTH = [8, 8, 8, 4, 4, 4, 4, 1, 1, 1, 1]   // was [8, 1, ...]

The rule, greedy by BD per 1% wall against what each preset already dropped
(CfL + angle 0.031, coefficient breakout 0.041 -- the frontier `speed.rs`
documents):

* preset 0 keeps 8 -- depth 4 costs film B 0.7 on BOTH columns. Pins unmoved.
* presets 3..6 take 4 -- best real-content arm at 3 (every row down), film B
  -0.7/-0.6 at 6, for no wall this box can see: >= 0.1 BD points per 1% wall
  against a frontier of 0.04, and the cheaper of the two windows that pay.
* presets 1..2 take 8 (unmeasured): nearest measured neighbour is preset 0,
  whose optimum is 8, and the monotone rule forbids anything below preset 3's.
* presets 7..10 stay at 1 (unmeasured, per charter).

Side effect: lane-deltaq's `EC_AV1_DELTAQ` is no longer inert at presets 1..6
-- there is a map to read again. It still ships OFF, and its witness' fire
count is now gated on the window (`speed::at(&TPL_DEPTH) > 1`) rather than on
`speed() == 0`.

## Invariants

* `EC_COMP_MISMATCH=1` at presets 0, 3, 6 over
  `tile_bytes_do_not_depend_on_the_thread_count --include-ignored` and
  `the_facade_codes_the_same_bytes_as_encode_sequence`: all six pass, 0
  mismatch lines (`lanes/tw-inv2.log`). The FIRST attempt used `--exact` with
  a bare test name and matched 0 tests -- a green "ok. 0 passed" that proved
  nothing (`lanes/tw-inv.log`, kept as the record of that miss).
* `every_speed_preset_decodes_sample_exact_through_both_decoders --ignored`
  under `EC_COMP_MISMATCH=1`: pass, 0 lines.
* `cargo check --workspace --all-targets`: 0 errors, 0 ec-av1 warnings.
