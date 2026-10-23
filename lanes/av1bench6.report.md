# lane av1bench6: re-check of the ec-av1 1080p30 decode sweep on a quiet machine

## Goal

The av1decbench section (lanes/bench.md) measured 1080p30 AV1 decode at 1.00x realtime with 6
threads, slower than 4 threads (3.41x). That row was taken under load 12 to 16 on 12 cores. This
lane repeats the sweep at 1, 2, 4 and 6 threads, gated on a quiet machine, to check whether the 6-thread
result was load noise. No code was changed.

Base: `f5eb855b` (main). Lane branch: `lane/av1bench6`.

## Method

- Clip: `~/.cache/av1decbench-clip/t1080.obu` (same stream as av1decbench and av1threads).
- Build: `CARGO_TARGET_DIR=~/.cache/tgt-av1bench6 TMPDIR=~/.cache/tmp-av1bench6 cargo build --release -j6 -p ec-av1 --example decode_bench` (ok).
- Timing: `EC_AV1_THREADS=n decode_bench t1080.obu` (no `--raw`), 5 runs per row, best fps reported.
- Gate: a row starts only when `/proc/loadavg` 1-minute value is below 3. Waits were in `sleep 290` steps,
  capped at 1 hour. The 1-thread row first waited 580 s (load 12.5, then 18.7, then 3.6) and started at load 2.88.
  The 2, 4 and 6-thread rows started at 2.06, 1.98 and 1.83 without waiting.
- md5: `decode_bench --raw | md5sum` after the timing runs, per row.
- Script: `~/.cache/tmp-av1bench6/row.sh` (outside the repo). Log: `~/.cache/tmp-av1bench6/bench.log`.

## Results

| threads | best fps | realtime x | median fps | load1 at start | md5 |
|---|---|---|---|---|---|
| 1 | 68.61 | 2.29x | 68.59 | 2.88 | 3adc24bb024913aab921dc8575b8be8b (ffmpeg) |
| 2 | 105.20 | 3.51x | 104.89 | 2.06 | same |
| 4 | 117.86 | 3.93x | 117.41 | 1.98 | same |
| 6 | 120.97 | 4.03x | 120.04 | 1.83 | same |

The 5-minute load average was 8 to 12 during the sweep, so the machine was still settling from the
earlier busy period. Per-run load (1-minute, before each run) stayed between 1.8 and 2.9 for every run.

## Findings

- The 6-thread row is faster than 4 threads here (120.97 vs 117.86 fps). The earlier 1.00x result
  was load noise on the earlier run, not a decoder property.
- The md5 at every thread count equals ffmpeg's decode (3adc24bb024913aab921dc8575b8be8b).
- The 6-thread profile step was not needed, because 6 threads is not slower than 4 on a quiet machine.
  No `perf record` was taken.
- Scaling flattens above 2 threads: 2 to 4 threads gives +12%, and 4 to 6 gives +2.6%. The 12-core
  machine is shared, so the upper threads may compete with other tenants even when the 1-minute load is low.
  A higher-fidelity check would use a longer clip and several quiet windows.

## Verify

| command | result |
|---|---|
| `cargo build --release -j6 -p ec-av1 --example decode_bench` (CARGO_TARGET_DIR set as above) | ok |
| `EC_AV1_THREADS={1,2,4,6} decode_bench t1080.obu`, best of 5 | 68.61 / 105.20 / 117.86 / 120.97 fps |
| `EC_AV1_THREADS={1,2,4,6} decode_bench t1080.obu --raw \| md5sum` | all 3adc24bb024913aab921dc8575b8be8b |

## Left

- No code changes in this lane.
- A longer clip and repeated quiet windows would tighten the 4-to-6 comparison, which is within a few percent.
