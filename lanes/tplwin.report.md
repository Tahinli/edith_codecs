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

(pending)
