# lane-rc — the rate loop's level model

`crates/ec-av1/src/encoder.rs` only (`RateLoop`, `gop_shape`, their tests).

## The defect (lane-pyr6's debt)

`RateLoop::new` split a mini-GOP as `m / (m - 1 + ARF_WEIGHT)`: ONE hidden
frame per group, priced at a flat two leaves. Since the mid level landed a
group codes TWO hidden frames at different q offsets, and a hidden frame is
worth far more than two leaves. The two errors cancelled, which is why fixing
either alone made the loop worse.

## The fitted weight function

Census: `EC_ENC_SS=<gate seek> EC_ENC_OUT=<dir> enc_probe <film> gate 0 48
110,150,190` then `EC_AV1_BITCENSUS=1 syntax_census <dir>/ours-q<q>.obu`,
film A (1080p, seek 00:35:00) and film B (2160p, seek 00:40:00). 48 pictures =
1 key + 6 top ARF + 6 mid + 35 leaves at the default pyramid
(8 : -32 : +12 : -8 : -48). Film B q150 reproduces `lanes/census-longgop-v2.md`
byte for byte (90674 B stream, key 19321 / ARF 8021 / mid 1722 / leaf 335).

Bytes per frame, and the ratio to that stream's own leaf:

| film | q | key (dq 60) | ARF (dq 44) | mid (dq 20) | leaf |
|---|---|---|---|---|---|
| A | 110 | 65594 (13.9x) | 29421 (6.3x) | 11023 (2.3x) | 4706 |
| A | 150 | 39722 (19.9x) | 13668 (6.9x) |  4730 (2.4x) | 1994 |
| A | 190 | 22621 (26.3x) |  6092 (7.1x) |  2194 (2.6x) |  860 |
| B | 110 | 37854 (25.2x) | 22798 (15.2x) | 6570 (4.4x) | 1504 |
| B | 150 | 19321 (57.7x) |  8021 (23.9x) | 1722 (5.1x) |  335 |
| B | 190 |  9733 (76.0x) |  2414 (18.9x) |  465 (3.6x) |  128 |

`dq` is `leaf_q_offset - own offset`. Least squares through the origin on the
12 INTER points (mid, ARF; both films, all three q) gives

    weight(offset) = exp(0.0557 * (leaf_q_offset - offset))     WEIGHT_LEVEL_GAIN

i.e. `d(ln bytes)/d(q_idx) = -0.0557`, three and a half times the controller's
own `GAIN` slope (0.0154), because a deeper level also carries the residual of
every frame that predicts off it. The six KEY points, divided by that same
function at `dq` 60, leave a residual of geometric mean 1.08 (spread 0.47 film
A q110 .. 2.68 film B q190), which is `KEY_WEIGHT`.

The run's plan is then `gop_shape` (the hidden frames a run really codes, by
offset, plus the leaf count -- mid needs 3 leaves and a run > 1.5 mini-GOPs,
quarters need 7 leaves and a mid) priced through `weight`, so the plan spends
`gop` frame targets exactly.

Two things the model alone cannot price, both re-planned from measurement
(`RateLoop::replan`, reset by every key frame so no debt outlives a run):
the KEY, coded at the caller's seed `q` before anything is measured (0.47x to
2.68x the model above), and any level whose `q` has hit the floor.

## The landed table (`bitrate_target_lands_within_5_percent_over_48_frames`)

48 frames, 640x384 clip, 24 fps. Flat arm is untouched by this diff (the model
is pyramid-only) and is shown for reference.

| bps | flat before | flat after | pyramid before | pyramid after |
|---|---|---|---|---|
| 384k  | -    | -0.4% | -    | +3.0% |
| 768k  | -1.8% | -1.8% | -0.4% | -0.0% |
| 1536k | -3.0% | -3.0% | -6.3% | +0.2% |
| 2M    | -    | -2.8% | -    | -4.4% |

Tolerance SHIPPED: +-5% (was +-10%), on all 8 arms.

CEILING, stated rather than hidden: the charter's 3 Mbit/s arm is not
reachable on this clip under the default pyramid and is NOT in the test. At
3 Mbit/s the hidden slot already codes at `q_idx` 1 (22863 bytes a frame, the
same figure it reaches at 1.5 Mbit/s) and the leaves sit at 1 + 12
`leaf_q_offset`; the whole stream tops out near 2.4 Mbit/s however the budget
is split. The top arm is 2 Mbit/s, inside the reach, and its -4.4% is that
ceiling showing through. Lifting it is a pyramid-shape change (the offsets
would have to collapse as `q` approaches the floor), out of this lane's scope.
