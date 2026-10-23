# Lane bench log

Speed rows: 1080p30 encode, 4 s noisy testsrc2 (120 pictures), 4 Mb/s, `h264_clip_encode`
through `scripts/remote-run.sh --host a`. realtime x = fps / 30. "before" is main
(d50d9476, with the Intra_8x8 and P-intra gate settings forced off via
`EC_H264_SKIP_I8=0 EC_H264_INTRA_GATE=0`, which reproduces main's bitstream). "after" is
`lane/h264encspeed` with its default Fast settings. Both ran in one job on the same host.

| date | crate | clip | host | threads | fps | realtime x |
|---|---|---|---|---|---|---|
| 2026-10-17 | ec-h264 (before) | 1080p30 noisy testsrc2 4 s | a | 1 | 0.94 | 0.031 |
| 2026-10-17 | ec-h264 (before) | 1080p30 noisy testsrc2 4 s | a | 2 | 1.52 | 0.051 |
| 2026-10-17 | ec-h264 (before) | 1080p30 noisy testsrc2 4 s | a | 4 | 1.70 | 0.057 |
| 2026-10-17 | ec-h264 (before) | 1080p30 noisy testsrc2 4 s | a | 6 | 1.48 | 0.049 |
| 2026-10-17 | ec-h264 (after) | 1080p30 noisy testsrc2 4 s | a | 1 | 2.07 | 0.069 |
| 2026-10-17 | ec-h264 (after) | 1080p30 noisy testsrc2 4 s | a | 2 | 2.68 | 0.089 |
| 2026-10-17 | ec-h264 (after) | 1080p30 noisy testsrc2 4 s | a | 4 | 2.74 | 0.091 |
| 2026-10-17 | ec-h264 (after) | 1080p30 noisy testsrc2 4 s | a | 6 | 3.05 | 0.102 |

Process CPU seconds for the same 120 pictures (`cpu_user`): before 126 / 134 / 135 / 134 s at
1 / 2 / 4 / 6 threads; after 58 / 61 / 62 / 61 s. The host had load 4 to 8 on 4 vCPUs.

Per-variant single-thread CPU seconds for 60 pictures (2 s, 1080p30, 1 thread, 4 Mb/s), on
hosts b and c during the sweep. Noisy is the testsrc2 with temporal noise; clean is plain
testsrc2. These are relative numbers within one batch:

| variant | BD-rate vs x264 veryfast (%) | noisy CPU s | clean CPU s |
|---|---|---|---|
| main (before) | 11.01 | 61.7 to 63.3 | 47.5 to 47.9 |
| Intra_8x8 off | 10.33 | 46.2 | 32.5 |
| **P-intra gate 2.0, Intra_8x8 on (shipped)** | **10.57** | **28.7** | **39.7** |
| Intra_8x8 off + gate 2.0 | 10.08 | 29.8 | 30.4 |
| Intra_4x4 1 candidate + Intra_8x8 off | 10.92 | 43.0 | 31.2 |
| no intra in P (bound, not shipped) | 8.72 | 30.4 | 21.1 |

## lane vp9speed: ec-vp9 1080p30 profile 0 decode

Host b (4 vCPU, shared with other lanes, load 1 to 4 during the runs). Before is `e7234d33`
(main), after is `lane/vp9speed`. Both measured in one session with `decode_ivf`. fps is the
decode-only rate: frames divided by the time spent inside `Decoder::decode`. The digest and
output writes are harness work and are no longer counted. Each value is the mean of two runs.

| date | crate | clip | host | threads | fps | realtime x |
|---|---|---|---|---|---|---|
| 2026-10-17 | ec-vp9 (before) | tile1080.ivf 1080p30 | b | 1 | 18.87 | 0.63 |
| 2026-10-17 | ec-vp9 (before) | tile1080.ivf 1080p30 | b | 2 | 21.51 | 0.72 |
| 2026-10-17 | ec-vp9 (before) | tile1080.ivf 1080p30 | b | 4 | 24.05 | 0.80 |
| 2026-10-17 | ec-vp9 (after) | tile1080.ivf 1080p30 | b | 1 | 29.61 | 0.99 |
| 2026-10-17 | ec-vp9 (after) | tile1080.ivf 1080p30 | b | 2 | 39.75 | 1.33 |
| 2026-10-17 | ec-vp9 (after) | tile1080.ivf 1080p30 | b | 4 | 49.58 | 1.65 |
| 2026-10-17 | ec-vp9 (before) | libvpx 1080p30 4 tile cols, 10 s | b | 1 | 45.79 | 1.53 |
| 2026-10-17 | ec-vp9 (before) | libvpx 1080p30 4 tile cols, 10 s | b | 2 | 44.00 | 1.47 |
| 2026-10-17 | ec-vp9 (before) | libvpx 1080p30 4 tile cols, 10 s | b | 4 | 42.57 | 1.42 |
| 2026-10-17 | ec-vp9 (after) | libvpx 1080p30 4 tile cols, 10 s | b | 1 | 58.42 | 1.95 |
| 2026-10-17 | ec-vp9 (after) | libvpx 1080p30 4 tile cols, 10 s | b | 2 | 61.55 | 2.05 |
| 2026-10-17 | ec-vp9 (after) | libvpx 1080p30 4 tile cols, 10 s | b | 4 | 68.90 | 2.30 |

The libvpx clip is `ffmpeg -f lavfi -i testsrc2=size=1920x1080:rate=30 -t 10 -c:v libvpx-vp9
-tile-columns 2 -row-mt 1 -b:v 4M -deadline realtime -cpu-used 8`, made by
`scripts/vp9-bench.sh`.

## lane av1threads: ec-av1 1080p30 decode

Realtime x = fps / 30 (1080p30 target). Rows marked "median" are the median of three timed
runs on the named host, run one after another. Each row is one decode of the same stream:
`testsrc2` 1920x1080, 30 fps, 10 s (300 frames), 8-bit 4:2:0, libaom cpu-used 8, row-mt 1,
2x2 tiles, 4 Mbps, muxed as a low-overhead OBU stream by `ffmpeg -f obu`.

| date | crate | clip | host | threads | fps | realtime x |
|---|---|---|---|---|---|---|
| 2026-10-17 | ec-av1 | 1080p30 8-bit 4:2:0, testsrc2 10 s (median) | a (4 cores, load 3-7) | 1 (`EC_AV1_THREADS=1`) | 21.0 | 0.70x |
| 2026-10-17 | ec-av1 | 1080p30 8-bit 4:2:0, testsrc2 10 s (median) | a (4 cores, load 3-7) | 2 (`EC_AV1_THREADS=2`) | 20.5 | 0.68x |
| 2026-10-17 | ec-av1 | 1080p30 8-bit 4:2:0, testsrc2 10 s (median) | a (4 cores, load 3-7) | 4 (`EC_AV1_THREADS=4`) | 20.7 | 0.69x |
| 2026-10-17 | ec-av1 | 1080p30 8-bit 4:2:0, testsrc2 10 s (median) | a (4 cores, load 3-7) | 6 (`EC_AV1_THREADS=6`) | 18.8 | 0.63x |
| 2026-10-17 | ec-av1 | 1080p30 8-bit 4:2:0, testsrc2 10 s (median) | a (4 cores, load 3-7) | default = min(4, 6) = 4 | 20.6 | 0.69x |
| 2026-10-17 | ec-av1 | 1080p30, NOT default, unchanged main code: 6 frame threads + `EC_AV1_RECON_THREADS=4` (single run; that knob is not bit-exact on a 4:2:0 qm stream) | a (4 cores) | 6 + recon 4 | 32.5 | 1.08x |
| 2026-10-17 | ec-av1 | 1080p30, NOT default, unchanged main code: 6 frame threads + `EC_AV1_RECON_THREADS=2` + `EC_AV1_FILTER_THREADS=4` (single run) | a (4 cores) | 6 + recon 2 + filter 4 | 34.5 | 1.15x |

## lane h264encspeed2

1080p30 encode, 10 s (300 pictures) noisy testsrc2 at 4 Mb/s, `h264-lane-check.sh fps`. "local" is the
user's 12-core machine (load 1 to 3), server rows are host a (4 vCPU, load 3 to 5). Clean rows are plain
testsrc2.

| date | crate | clip | host | threads | fps | realtime x |
|---|---|---|---|---|---|---|
| 2026-10-17 | ec-h264 (speed2) | 1080p30 noisy testsrc2 10 s | a | 1 | 5.58 | 0.186 |
| 2026-10-17 | ec-h264 (speed2) | 1080p30 noisy testsrc2 10 s | a | 2 | 7.31 | 0.244 |
| 2026-10-17 | ec-h264 (speed2) | 1080p30 noisy testsrc2 10 s | a | 4 | 8.45 | 0.282 |
| 2026-10-17 | ec-h264 (speed2) | 1080p30 noisy testsrc2 10 s | a | 6 | 8.54 | 0.285 |
| 2026-10-17 | ec-h264 (speed2) | 1080p30 noisy testsrc2 10 s | local | 1 | 12.33 | 0.411 |
| 2026-10-17 | ec-h264 (speed2) | 1080p30 noisy testsrc2 10 s | local | 2 | 18.16 | 0.605 |
| 2026-10-17 | ec-h264 (speed2) | 1080p30 noisy testsrc2 10 s | local | 4 | 34.83 | 1.161 |
| 2026-10-17 | ec-h264 (speed2) | 1080p30 noisy testsrc2 10 s | local | 6 | 41.72 | 1.391 |
| 2026-10-17 | ec-h264 (speed2) | 1080p30 clean testsrc2 10 s | local | 1 | 34.10 | 1.137 |
| 2026-10-17 | ec-h264 (speed2) | 1080p30 clean testsrc2 10 s | local | 6 | 93.68 | 3.123 |

## lane av1recon: ec-av1 1080p30 with the recon wavefront on by default

Same stream and host as the av1threads section (a: 4 cores, two other tenants' processes
holding about 2 cores, load 3 to 5, so figures swing by +-15% between runs). Settings are the
decoder defaults: only `EC_AV1_THREADS=n` is set; the recon/filter split comes from
`autothreads::split(n)` (n=2: recon 2 / filter 1, n=4: 3 / 2, n=6: 4 / 3). Three runs each,
median in the table. md5 of `decode_bench --raw` equals ffmpeg's decode
(`3adc24bb024913aab921dc8575b8be8b`) at n = 1, 4 and 6.

| date | crate | clip | host | threads | fps (3 runs) | median fps | realtime x |
|---|---|---|---|---|---|---|---|
| 2026-10-17 | ec-av1 | 1080p30 8-bit 4:2:0, testsrc2 10 s | a | 1 (serial) | 19.5 / 22.0 / 22.0 | 22.0 | 0.73x |
| 2026-10-17 | ec-av1 | 1080p30 8-bit 4:2:0, testsrc2 10 s | a | 2 (recon 2, filter 1) | 27.7 / 29.9 / 28.5 | 28.5 | 0.95x |
| 2026-10-17 | ec-av1 | 1080p30 8-bit 4:2:0, testsrc2 10 s | a | 4 (recon 3, filter 2) | 31.3 / 27.7 / 31.0 | 31.0 | 1.03x |
| 2026-10-17 | ec-av1 | 1080p30 8-bit 4:2:0, testsrc2 10 s | a | 6 (recon 4, filter 3) | 29.6 / 28.0 / 28.6 | 28.6 | 0.95x |

Explicit-knob peaks on the same host in the same window (frame workers do not matter):
recon 3 + filter 2 reached 35.2 fps (T=1) and 32.3 (T=4); recon 4 + filter 3 reached 34.0 (T=1).

| 2026-10-17 | ec-h265 (before, main fbfe7c9e) | 1080p30 testsrc2 scroll 30 frames, QP 30, GOP 8 | b | 1 | 0.79 | 0.026 |
| 2026-10-17 | ec-h265 (before, main fbfe7c9e) | 1080p30 testsrc2 scroll 30 frames, QP 30, GOP 8 | b | 6 | 1.88 | 0.063 |
| 2026-10-17 | ec-h265 (hevcencspeed3) | 1080p30 testsrc2 scroll 30 frames, QP 30, GOP 8 | b | 1 | 1.07 | 0.036 |
| 2026-10-17 | ec-h265 (hevcencspeed3) | 1080p30 testsrc2 scroll 30 frames, QP 30, GOP 8 | b | 4 | 2.41 | 0.080 |
| 2026-10-17 | ec-h265 (hevcencspeed3) | 1080p30 testsrc2 scroll 30 frames, QP 30, GOP 8 | b | 6 | 2.45 | 0.082 |
| 2026-10-17 | ec-h265 (before, main fbfe7c9e) | 1080p30 testsrc2 scroll 30 frames, QP 30, GOP 8 | local | 1 | 1.92 | 0.064 |
| 2026-10-17 | ec-h265 (before, main fbfe7c9e) | 1080p30 testsrc2 scroll 30 frames, QP 30, GOP 8 | local | 6 | 5.57 | 0.186 |
| 2026-10-17 | ec-h265 (hevcencspeed3) | 1080p30 testsrc2 scroll 30 frames, QP 30, GOP 8 | local | 1 | 2.50 | 0.083 |
| 2026-10-17 | ec-h265 (hevcencspeed3) | 1080p30 testsrc2 scroll 30 frames, QP 30, GOP 8 | local | 6 | 6.90 | 0.230 |

## lane hevcdecbench: ec-h265 1080p30 / 4K30 decode

Bench: `decode_bench` (`crates/ec-h265/examples/decode_bench.rs`), release build
(`cargo build --release -j6 -p ec-h265 --example decode_bench`), local host. Each row is the median of
3 separate process runs, one timed decode each (`EC_BENCH_RUNS=1`). Clips from
`~/.cache/hevcperf-fixtures/` (120 frames each). Realtime x = 30 fps / ms per frame. Checksums match
across thread counts (1080p `2bfe18f415b7408d`, 4K `6d7084bb34f40906`).

| clip | resolution | threads | runs (ms/frame) | median ms/frame | fps | realtime x | host | date |
|---|---|---|---|---|---|---|---|---|
| hevc1080_moving | 1920x1080 30 fps | 1 | 45.50 / 31.38 / 33.40 | 33.40 | 29.9 | 1.00x | tFedora | 2026-10-17 |
| hevc1080_moving | 1920x1080 30 fps | 6 | 13.94 / 6.43 / 6.24 | 6.43 | 155.5 | 5.18x | tFedora | 2026-10-17 |
| hevc4k_moving | 3840x2160 30 fps | 1 | 88.59 / 82.46 / 91.15 | 88.59 | 11.3 | 0.38x | tFedora | 2026-10-17 |
| hevc4k_moving | 3840x2160 30 fps | 6 | 21.64 / 24.85 / 21.96 | 21.96 | 45.5 | 1.52x | tFedora | 2026-10-17 |

Criteria (local, at <= 6 threads):
- 1080p30 >= 2x realtime: **met** at 6 threads (5.18x, 6.43 ms/frame). Not met serially (1.00x at 1 thread).
- 4K30 >= 1x realtime: **met** at 6 threads (1.52x, 21.96 ms/frame). Not met serially (0.38x at 1 thread).

The 6-thread medians are faster than the figures in `lanes/hevcframethreads.report.md` (7.36 and
27.11 ms/frame); this machine state differs between runs. The first 1080p 6-thread run (13.94 ms) is
an outlier and the median discards it.

## lane h264encpool
1080p30 encode, 4 s noisy testsrc2 at 4 Mb/s (120 pictures), `h264_clip_encode`, local 12-core machine
(load 7 to 32 during runs). Three interleaved rounds per row, median fps. "before" is main, "after"
is `lane/h264encpool` (persistent band workers, byte-identical output).

| date | crate | clip | host | threads | fps | realtime x |
|---|---|---|---|---|---|---|
| 2026-10-17 | ec-h264 (before) | 1080p30 noisy testsrc2 4 s | local | 1 | 12.54 | 0.418 |
| 2026-10-17 | ec-h264 (after) | 1080p30 noisy testsrc2 4 s | local | 1 | 12.00 | 0.400 |
| 2026-10-17 | ec-h264 (before) | 1080p30 noisy testsrc2 4 s | local | 6 | 40.78 | 1.359 |
| 2026-10-17 | ec-h264 (after) | 1080p30 noisy testsrc2 4 s | local | 6 | 41.38 | 1.379 |

## lane av1decbench: ec-av1 1080p30 decode, local

Clip: `t1080.obu`, the same stream as the av1threads section (`ffmpeg -f lavfi -i testsrc2=size=1920x1080:rate=30 -t 10 -c:v libaom-av1 -cpu-used 8 -row-mt 1 -tiles 2x2 -b:v 4M -f obu`), 7,120,251 bytes, 300 frames, 622.08 Mpx, 8-bit 4:2:0. Built locally: `cargo build --release -j6 -p ec-av1 --example decode_bench`. Run locally on the 12-core machine with `EC_AV1_THREADS=n` only (recon/filter defaults, no `EC_AV1_RECON_THREADS` / `EC_AV1_FILTER_THREADS`). Three timed runs per row (no `--raw`), median fps; ms/frame = 1000 / median fps. Realtime x = fps / 30. Load at start: 13.05 (1 min) on 12 cores, 16.4 during the runs (other tenants); runs are noisy.

| date | crate | clip | host | threads | runs (fps) | median fps | ms/frame | realtime x | md5 |
|---|---|---|---|---|---|---|---|---|---|
| 2026-10-17 | ec-av1 | 1080p30 8-bit 4:2:0, testsrc2 10 s | local (tFedora) | 1 (`EC_AV1_THREADS=1`) | 32.87 / 60.20 / 63.17 | 60.20 | 16.61 | 2.01x | ok (`3adc24bb024913aab921dc8575b8be8b`, = ffmpeg) |
| 2026-10-17 | ec-av1 | 1080p30 8-bit 4:2:0, testsrc2 10 s | local (tFedora) | 2 (`EC_AV1_THREADS=2`) | 88.18 / 96.87 / 93.61 | 93.61 | 10.68 | 3.12x | ok |
| 2026-10-17 | ec-av1 | 1080p30 8-bit 4:2:0, testsrc2 10 s | local (tFedora) | 4 (`EC_AV1_THREADS=4`) | 102.31 / 109.06 / 44.64 | 102.31 | 9.77 | 3.41x | ok |
| 2026-10-17 | ec-av1 | 1080p30 8-bit 4:2:0, testsrc2 10 s | local (tFedora) | 6 (`EC_AV1_THREADS=6`) | 32.16 / 30.12 / 25.94 | 30.12 | 33.20 | 1.00x | ok |

md5 was checked with `decode_bench --raw | md5sum` at threads 1, 2, 4 and 6; all four equal ffmpeg's decode.

Criterion (1080p30 8-bit at >= realtime with EC_AV1_THREADS > 1, local): **met** at 2 threads (3.12x) and 4 threads (3.41x). At 6 threads the median was 1.00x, but all three 6-thread runs were slower than the 4-thread runs in this window, while load was 12 to 16 on 12 cores; the 6-thread figure is not a reliable measure of the decoder, so it is recorded as measured. The 4-thread median is the middle run; its best run (109.06 fps) and worst (44.64 fps) show the spread.
## lane hevcencspeed4

Local host (Ryzen 5 9600, 12 hardware threads shared with other jobs, load average 5-9 during the runs), `scripts/hevc-thread-bench.sh`, 1080p30 testsrc2 scroll 30 frames, QP 30, GOP 8, best of 5-10 runs. Streams byte-identical across thread counts.

| date | codec | clip | host | threads | fps | realtime x |
|---|---|---|---|---|---|---|
| 2026-10-17 | ec-h265 (hevcencspeed4) | 1080p30 testsrc2 scroll 30 frames, QP 30, GOP 8 | local | 1 | 12.40 | 0.413 |
| 2026-10-17 | ec-h265 (hevcencspeed4) | 1080p30 testsrc2 scroll 30 frames, QP 30, GOP 8 | local | 2 | 21.35 | 0.712 |
| 2026-10-17 | ec-h265 (hevcencspeed4) | 1080p30 testsrc2 scroll 30 frames, QP 30, GOP 8 | local | 3 | 25.47 | 0.849 |
| 2026-10-17 | ec-h265 (hevcencspeed4) | 1080p30 testsrc2 scroll 30 frames, QP 30, GOP 8 | local | 6 | 28.60 | 0.953 |
## lane h264encclone
1080p30 encode, 4 s noisy testsrc2 at 4 Mb/s and at QP 12 (120 pictures), `h264_clip_encode`,
local 12-core machine. The machine was shared with other lanes: load average 12 to 72 during the
runs, so wall-clock fps is noise-dominated. Three interleaved rounds per row, median fps. "before"
is `2a1f421d` (main at lane base), "after" is `lane/h264encclone` (no per-candidate writer clone,
byte-identical output). Local release build with `-j6`, sccache disabled.

| date | crate | clip | host | threads | rate | fps before | fps after | realtime x before | realtime x after |
|---|---|---|---|---|---|---|---|---|---|
| 2026-10-17 | ec-h264 | 1080p30 noisy testsrc2 4 s | local | 1 | 4 Mb/s | 13.28 | 13.66 | 0.443 | 0.455 |
| 2026-10-17 | ec-h264 | 1080p30 noisy testsrc2 4 s | local | 6 | 4 Mb/s | 43.37 | 34.71 | 1.446 | 1.157 |
| 2026-10-17 | ec-h264 | 1080p30 noisy testsrc2 4 s | local | 1 | QP 12 | 1.21 | 0.90 | 0.040 | 0.030 |
| 2026-10-17 | ec-h264 | 1080p30 noisy testsrc2 4 s | local | 6 | QP 12 | 4.19 | 2.28 | 0.140 | 0.076 |

Per-round fps (round 1, 2, 3):
- 4 Mb/s, 1 thread: before 13.33, 12.90, 13.28; after 13.66, 13.66, 12.07.
- 4 Mb/s, 6 threads: before 44.70, 43.37, 32.97; after 34.71, 44.01, 31.73.
- QP 12, 1 thread: before 1.22, 1.21, 0.80; after 1.10, 0.76, 0.90.
- QP 12, 6 threads: before 4.19, 4.82, 2.85; after 5.37, 2.28, 1.44.

These wall-clock rows do not show a usable speed difference. Cycle counts, which do not depend on
the wall-clock load as much, were measured with `perf stat -e cycles:u,instructions:u` on the first
60 frames at QP 12, 1 thread, two alternating rounds: cycles before 275.9 G and 293.6 G, after
267.5 G and 260.8 G; instructions 832.8 G before and 832.7 G after. `perf record` shows `memmove`
at 2.2% of cycles before and 1.6% after. The removed copy was a small share of the encode, so the
expected gain is a few percent at high bitrate, not more.

## lane hevcencspeed5

Local host (12 hardware threads shared with other jobs, load average 24-50 during the runs), `scripts/hevc-thread-bench.sh`, 1080p30 testsrc2 scroll 30 frames, QP 30, GOP 8, best of 6 runs. Streams byte-identical across thread counts (629083 bytes, same as before the change).

| date | codec | clip | host | threads | fps | realtime x |
|---|---|---|---|---|---|---|
| 2026-10-17 | ec-h265 (hevcencspeed5) | 1080p30 testsrc2 scroll 30 frames, QP 30, GOP 8 | local | 1 | 12.36 | 0.412 |
| 2026-10-17 | ec-h265 (hevcencspeed5) | 1080p30 testsrc2 scroll 30 frames, QP 30, GOP 8 | local | 2 | 19.33 | 0.644 |
| 2026-10-17 | ec-h265 (hevcencspeed5) | 1080p30 testsrc2 scroll 30 frames, QP 30, GOP 8 | local | 3 | 24.12 | 0.804 |
| 2026-10-17 | ec-h265 (hevcencspeed5) | 1080p30 testsrc2 scroll 30 frames, QP 30, GOP 8 | local | 6 | 40.16 | 1.339 |

## lane hevcencrcpar

Local host, `scripts/hevc-thread-bench.sh` in bitrate mode (`RC_KBPS=4000 FPS=30`, through the
`hevc_rc` example), 1920x1080 testsrc2 scroll, 180 frames, GOP 30 (six GOPs), best of 5 runs per
point. Both sides run on the same machine within minutes of each other: "main" is `8fb9fb6c` built
from a detached worktree, "new" is `lane/hevcencrcpar`. Load averages (1 min, at the start of each
round) were 0.10 and 1.03 in round 1 and 2.26 and 2.78 in round 2. The timing includes the recon
YUV write that `hevc_rc` makes, on both sides.

| date | codec | clip | host | threads | fps main | fps new | realtime x new |
|---|---|---|---|---|---|---|---|
| 2026-10-17 | ec-h265 (hevcencrcpar) | 1080p30 testsrc2 scroll 180 frames, 4000 kb/s, GOP 30 | local | 1 | 14.21 | 13.97 | 0.466 |
| 2026-10-17 | ec-h265 (hevcencrcpar) | 1080p30 testsrc2 scroll 180 frames, 4000 kb/s, GOP 30 | local | 6 | 36.87 | 51.86 | 1.729 |
| 2026-10-17 | ec-h265 (hevcencrcpar) | 1080p30 testsrc2 scroll 180 frames, 4000 kb/s, GOP 30 | local | 6 (round 2) | 36.62 | 51.37 | 1.712 |

The 1-thread row is one best-of-5 round (the other is the same clip in an earlier run); both
sides within noise. Streams: 2978622 bytes for the new build and 2996888 for main, identical across
thread counts on both. Constant-QP streams are unchanged: 1080p30, 30 frames, QP 30, GOP 8 gives
629083 bytes and the same md5 for main and new at 1 and 6 threads.

## lane av1bench6

Re-run of the av1decbench sweep on the same local 12-core machine and clip (`~/.cache/av1decbench-clip/t1080.obu`, 7,120,251 bytes, 300 frames, 622.08 Mpx, 1080p30 8-bit 4:2:0 testsrc2). Built with `CARGO_TARGET_DIR=~/.cache/tgt-av1bench6 cargo build --release -j6 -p ec-av1 --example decode_bench`. Each row is best of 5 timed runs (no `--raw`), `EC_AV1_THREADS=n` only. Each row started only when the 1-minute load was below 3; the load at each row start is in the table. Realtime x = fps / 30.

| date | crate | clip | host | threads | runs (fps) | best fps | realtime x | load1 at row start | md5 |
|---|---|---|---|---|---|---|---|---|---|
| 2026-10-17 | ec-av1 | 1080p30 8-bit 4:2:0, testsrc2 10 s | local (tFedora) | 1 | 68.59 / 68.61 / 68.59 / 68.38 / 68.51 | 68.61 | 2.29x | 2.88 | ok (`3adc24bb024913aab921dc8575b8be8b`, = ffmpeg) |
| 2026-10-17 | ec-av1 | 1080p30 8-bit 4:2:0, testsrc2 10 s | local (tFedora) | 2 | 104.17 / 105.16 / 104.13 / 104.89 / 105.20 | 105.20 | 3.51x | 2.06 | ok |
| 2026-10-17 | ec-av1 | 1080p30 8-bit 4:2:0, testsrc2 10 s | local (tFedora) | 4 | 117.68 / 117.41 / 117.37 / 116.73 / 117.86 | 117.86 | 3.93x | 1.98 | ok |
| 2026-10-17 | ec-av1 | 1080p30 8-bit 4:2:0, testsrc2 10 s | local (tFedora) | 6 | 120.97 / 120.88 / 120.04 / 119.38 / 119.96 | 120.97 | 4.03x | 1.83 | ok |

Median fps: 68.59 / 104.89 / 117.41 / 120.04 at 1 / 2 / 4 / 6 threads. The 6-thread row is no longer slower than 4 threads on a quiet machine (+2.6% best, +2.2% median), so the 6-thread 1.00x result in the av1decbench section was load noise, not decoder behaviour. The 4-to-6 gain is small, and 2-to-4 gives +12%.
## lane benchrows: local decode rows (ec-vp8, ec-vp9, ec-ac3, ec-h264)

Local 12-core machine, the DONE.md D7 exception. Each row is the best of 5 runs. A row starts
only when the 1-minute load is below 3; the load at the start of each row is in its column.
Clips are generated with ffmpeg 8.1.3 by `scripts/bench-rows-clips.sh` (not in the repo):
VP8 and VP9 are `testsrc2` 1920x1080 30 fps 10 s at 4 Mb/s (libvpx, realtime, cpu-used 8; VP9
with 4 tile columns); AC-3 is 60 s of six independent pink-noise channels, 5.1 layout, 448 kb/s.
The h264 clip is the first 10 s of the 1080p30 clip that `ec-bench` picks. Driver:
`scripts/bench-rows.sh`. Video realtime x = fps / 30. AC-3 realtime x = audio seconds /
decode seconds. md5 checks compare the whole decoded yuv420p stream with ffmpeg's decode.

| date | crate | clip | host | threads | load 1m at start | fps (best of 5) | realtime x | check |
|---|---|---|---|---|---|---|---|---|
| 2026-10-18 | ec-vp8 | 1080p30 libvpx 10 s, 4 Mb/s | local 12-core | 1 | 1.21 | 157.30 | 5.24x | md5 = ffmpeg |
| 2026-10-18 | ec-vp9 | 1080p30 profile 0 libvpx-vp9 10 s, 4 tile cols | local 12-core | 1 | 1.16 | 165.13 | 5.50x | md5 = ffmpeg |
| 2026-10-18 | ec-vp9 | 1080p30 profile 0 libvpx-vp9 10 s, 4 tile cols | local 12-core | 2 | 1.12 | 171.81 | 5.73x | md5 = ffmpeg |
| 2026-10-18 | ec-vp9 | 1080p30 profile 0 libvpx-vp9 10 s, 4 tile cols | local 12-core | 4 | 1.31 | 160.53 | 5.35x | md5 = ffmpeg |
| 2026-10-18 | ec-vp9 | 1080p30 profile 0 libvpx-vp9 10 s, 4 tile cols | local 12-core | 6 | 1.40 | 159.62 | 5.32x | md5 = ffmpeg |
| 2026-10-18 | ec-ac3 | AC-3 5.1 448 kb/s 60 s (5.1 layout) | local 12-core | 1 | 1.22 | - | 322.2x | decode errors 0; parity: min_corr=1.000000 tolerance=0.999 PASS (ac3benchrerun, main 654717d7; was 0.980963 FAIL before ac3parity) |
| 2026-10-18 | ec-h264 | 1080p30 film 10 s, ec-bench row (wall best 1720.0 ms, 302 frames) | local 12-core | 1 | 1.49 | 175.58 | 5.85x | cores pinned 1, decoder threads 1; loads of 5 runs: 1.49 1.27 1.15 1.16 1.16 |
| 2026-10-18 | ec-h264 | 1080p30 film 10 s, ec-bench row (wall best 1740.7 ms, 302 frames) | local 12-core | 1 | 1.17 | 173.49 | 5.78x | cores pinned 2, decoder threads 1; loads of 5 runs: 1.17 1.18 1.19 1.33 1.25 |
| 2026-10-18 | ec-h264 | 1080p30 film 10 s, ec-bench row (wall best 1714.9 ms, 302 frames) | local 12-core | 1 | 1.35 | 176.10 | 5.87x | cores pinned 4, decoder threads 1; loads of 5 runs: 1.35 1.81 1.73 1.59 1.43 |
| 2026-10-18 | ec-h264 | 1080p30 film 10 s, ec-bench row (wall best 1723.6 ms, 302 frames) | local 12-core | 1 | 1.46 | 175.21 | 5.84x | cores pinned 6, decoder threads 1; loads of 5 runs: 1.46 1.92 1.60 1.40 1.47 |

Notes:
- ec-vp9 threads: the decoder splits work by tile column. 2 threads is the best (171.8 fps);
  4 and 6 threads are no faster than 1 thread on this clip.
- ec-h264 threads: the decoder has no thread knob (threads live only in `enc/`), so the rows
  pin the same single-threaded `ec-bench` run to 1, 2, 4 and 6 cores. All four are within 2%.
  The bench.log:145 row (2.61x, 3850 ms) was not reproduced: this run gives 5.85x (1720 ms).
  Cause not identified (decoder and machine state differ from that run).
- ec-ac3 parity: PASSES since the ac3parity merge (dither is ffmpeg's AVLFG). Re-run on
  main 654717d7 (`ac3benchrerun`) with a freshly generated clip (same ffmpeg 8.1.3 command as
  above): per-channel correlation 1.000000 on all six channels, max abs err 1.3e-5, decoded
  on server host a. History: before ac3parity the same clip gave min_corr 0.980963
  (0.990 with `--dither off`, 0.981 with `--drc 0.0`); that residual was the zero-bit dither
  generator, not a decoder gain or channel error. Speed row (`ac3benchrerun`, local, best of
  5, load 1.22 at start): 322.2x realtime (runs 320.1 to 322.2x).
- ec-ac3 parity FAILS the existing tolerance (correlation >= 0.999 per channel). Measured
  against ffmpeg 8.1.3 decoding the same file: min channel correlation 0.981 (default
  options), 0.990 with `--dither off`, and 0.981 with `--drc 0.0`. Per-channel slope is
  about 0.981 and the residual is about 19% of the signal rms, far above dither-level
  error, so a decoder difference on this stream, with dither only part of it (0.981 to 0.990
  with dither off). Not fixed here (no
  code changes in this lane). The ec-ac3 DONE log entry used real_library samples, not
  this stream. Speed for the same row: 323x realtime.

## lane benchp1: local parse, demux and write rows (ec-subs, ec-ass, ec-pgs, ec-ogg, ec-riff)

Local 12-core machine, the DONE.md D7 exception (gate G6). Each row is the best of 5 runs, one
thread (these parsers and demuxers are single-threaded). The 1-minute load at the start of each
row is in its column; all were below 3. Clips are made by `scripts/bench-benchp1-clips.sh` with
ffmpeg 8.1.3 (not in the repo). The `.sup` input is written by `ec-pgs --example benchp1 gen`,
since ffmpeg has no PGS encoder. Binaries are built on server host a (`scripts/remote-run.sh`) and
run locally. Correctness checks ran on host a against ffprobe and ffmpeg (the check column).

Clips: SRT and WebVTT are 2 h of cues, 14400 cues each (ffmpeg converts the awk-written ASS
script, which has 20 styles and 14400 styled events). ASS is that script, 2.35 MB. PGS is 2 h of
display sets (7200 shows of a 64x32 object plus 7200 clears), 16.1 MB, 43200 segments. Ogg is
10 min of stereo pink noise, Vorbis q4 (6.1 MB) and Opus 128 kb/s (6.5 MB). WAV is 10 min of
24-bit 48 kHz 5.1 (six pink-noise channels joined as 5.1), 518.4 MB.

Subtitle and PGS rates use the file duration as media time (2 h = 7200 s). Ogg uses 600 s and
WAV uses 600 s. Each subtitle timed run repeats the parse until it takes at least 0.2 s (the
inner count is in the run output); the row is the best per-parse time.

| date | crate | clip | host | threads | load 1m at start | MB/s (best of 5) | realtime x | check |
|---|---|---|---|---|---|---|---|---|
| 2026-10-18 | ec-subs | SubRip 2 h, 14400 cues, 2.41 MB (`parse`) | local 12-core | 1 | 1.41 | 122.47 | 365136x | cues=14400 = ffprobe 14400 |
| 2026-10-18 | ec-subs | WebVTT 2 h, 14400 cues, 1.23 MB (`parse`) | local 12-core | 1 | 1.41 | 125.32 | 733198x | cues=14400 = ffprobe 14400 |
| 2026-10-18 | ec-ass | ASS 2 h, 14400 styled events, 2.35 MB (`parse`) | local 12-core | 1 | 1.41 | 104.15 | 319600x | cues=14400 = ffprobe 14400 |
| 2026-10-18 | ec-pgs | PGS 2 h, 14400 display sets, 16.1 MB (`decode`, one segment per push) | local 12-core | 1 | 1.59 | 0.98 | 437x | segments 43200 = ffprobe 43200; 14400 frames (one per display set); push errors 0 |
| 2026-10-18 | ec-ogg | Vorbis 10 min stereo q4, 6.1 MB (demux) | local 12-core | 1 | 1.42 | 494.08 | 48780x | 28140 packets, 6082163 bytes = ffprobe |
| 2026-10-18 | ec-ogg | Opus 10 min stereo 128 kb/s, 6.5 MB (demux) | local 12-core | 1 | 1.42 | 498.18 | 45802x | 30001 packets, 6515119 bytes = ffprobe |
| 2026-10-18 | ec-riff | WAV 24-bit 5.1 48 kHz 10 min, 518 MB (`WavReader` read_all_i32) | local 12-core | 1 | 1.42 | 1547.01 | 1790x | 172800000 samples = 600 s x 48000 x 6 (ffprobe) |
| 2026-10-18 | ec-riff | WAV 24-bit 5.1 write, RIFF header (`WavWriter`) | local 12-core | 1 | 1.42 | 1431.72 | 1657x | ffmpeg s24le md5 of output = source md5 (b405de61) |
| 2026-10-18 | ec-riff | RF64 24-bit 5.1 write (`WavWriter`, rf64_threshold 0) | local 12-core | 1 | 1.47 | 1495.72 | 1731x | RF64 magic; ffmpeg s24le md5 = source md5 (b405de61) |

Notes:
- ec-pgs: `push` queues every finished display set as a full 1920x1080 canvas (about 8 MB)
  until `take_frame` drains it. The bench feeds one segment per push and drains after each, so
  peak RSS stays at about 26 MB. Feeding the whole 2 h file in one push held about 1 GB per 120
  sets and was OOM-killed on host a. The 16.5 s local row is about 3x faster than the 52 s
  host-a run of the same binary. The server was busier, so the local number is the one recorded.
- ec-riff write times are to the page cache (the output file is overwritten 5 times and not
  fsynced), so they measure encode plus buffered writes, not disk speed.
- ec-ogg: packet count and byte total match ffprobe exactly for both files (header packets are
  not returned as audio packets by either tool).
- Clips and binaries live outside the repo: `~/.cache/benchp1-clips`, `~/.cache/benchp1-bin`.

## lane pgsbudget: ec-pgs decode after the bounded queue (local)

Same synthetic 2 h `.sup` as `lane benchp1` (16.1 MB, 43200 segments, 14400 display sets; made by
`benchp1 gen` on host a). Local 12-core machine, one thread, best of 5 single runs. The start
load was 1.47; the 1-minute load after the five runs read 3.6, because the runs themselves are
single-thread jobs. Binary built on server host a from the lane branch. Output check: frames=14400,
push_errors=0, same as before.

| date | crate | clip | host | threads | load 1m at start | MB/s (best of 5) | realtime x | check |
|---|---|---|---|---|---|---|---|---|
| 2026-10-18 | ec-pgs | PGS 2 h, 14400 display sets, 16.1 MB (`decode`, per-segment drain) | local 12-core | 1 | 1.47 | 0.99 | 442x | frames=14400 push_errors=0 |

Before this lane (`lane benchp1`): 0.98 MB/s, 437x, 16.5 s. The time did not move. Measured
cause: a probe that allocates and returns each canvas but skips drawing runs in 16.3 s, so the
canvas allocation and zeroing (8.3 MB per display set, about 119 GB in all) is the cost.

## lane benchaudio: local audio decode and encode rows (ec-aac, ec-opus, ec-vorbis, ec-mp3, ec-truehd, ec-mp4)

Local 12-core machine, the DONE.md D7 exception (gate G6). Each row is the best of 5 runs in one
process (`examples/bench.rs` in each crate; the speed is the 5-run minimum wall time), one thread,
built with `cargo build --release -j6`. The code under test is `edea19ce` (no crate source
changed; the lane adds only the six examples, the script and this section). Every row starts only
when the 1-minute load is below 3; the load at the start of each row is in its column. The whole
run is `scripts/bench-benchaudio.sh`, which generates the clips with the local ffmpeg 8.1.3 and
writes these rows. Clips are synthetic and live outside the repo in `~/.cache/benchaudio-clips`.

Clips (all 60 s): stereo is 48 kHz tones with a slow AM plus two independent pink-noise channels
(`aevalsrc` + `anoisesrc`); 5.1 is six tones plus six pink-noise channels, TrueHD 24-bit; video is
`testsrc2` 1920x1080 30 fps with libx264 veryfast 4 Mb/s and AAC 128k stereo.
Realtime x = audio seconds / best time. Decoders are checked against ffmpeg 8.1.3 on the decoded
output: correlation is the minimum per-channel Pearson correlation after the best lag (+-4096
samples), computed on 2 s at the 10 s mark; TrueHD and the mp4 payloads are compared byte by byte
(md5).

| date | crate | clip | host | threads | load 1m at start | best of 5 | realtime x / rate | check |
|---|---|---|---|---|---|---|---|---|
| 2026-10-18 | ec-aac decode | AAC-LC stereo 128 kb/s 60 s (libfdk_aac, ADTS) | local 12-core | 1 | 0.11 | 0.0837 s | 717x | 2882560 frames/ch = ffmpeg; corr 1.000000 vs ffmpeg native aac, lag 0 |
| 2026-10-18 | ec-aac decode | HE-AAC v1 stereo, implicit SBR, 54.95 s decoded (repo fixture x28, see notes) | local 12-core | 1 | 1.06 | 0.5528 s | 99.4x | 2637824 frames/ch = ffmpeg; corr 0.999998 vs ffmpeg native aac, lag 0 |
| 2026-10-18 | ec-aac encode | AAC-LC stereo 128 kb/s from 60 s PCM (ADTS out), lane aacencnoise | local 12-core | 1 | 0.85 | 0.7955 s | 75x | 128.0 kb/s; ffmpeg decode of our ADTS corr 0.997942 vs source (lag 1024), was 0.974786 before the lane; see notes |
| 2026-10-18 | ec-aac encode | AAC-LC stereo 192 kb/s from 60 s PCM (ADTS out), lane aacencnoise | local 12-core | 1 | 0.85 | 0.8446 s | 71x | 192.1 kb/s; ffmpeg decode of our ADTS corr 0.998958 vs source (lag 1024) |
| 2026-10-18 | ec-opus decode | Opus 48k stereo 96 kb/s 60 s (libopus, Ogg) | local 12-core | 1 | 1.05 | 0.0878 s | 684x | corr 1.000000 vs ffmpeg Opus decode, lag 312 (our output keeps the pre-skip) |
| 2026-10-18 | ec-opus encode | Opus 48k stereo 96 kb/s 60 s (vbr off, 20 ms frames, Ogg) | local 12-core | 1 | 1.05 | 0.1770 s | 339x | 96.0 kb/s; ffmpeg decode of our Ogg corr 0.998553 vs source |
| 2026-10-18 | ec-vorbis decode | Vorbis q5 stereo 60 s (libvorbis, Ogg) | local 12-core | 1 | 1.05 | 0.0937 s | 640x | corr 1.000000 vs ffmpeg Vorbis decode, lag 0 |
| 2026-10-18 | ec-mp3 encode | MP3 192 kb/s CBR stereo 48 kHz 60 s (raw Layer III) | local 12-core | 1 | 1.05 | 0.5145 s | 117x | 192.2 kb/s; ffmpeg decode of our MP3 corr 0.999972 vs source, lag 0 |
| 2026-10-18 | ec-truehd decode | TrueHD 5.1 24-bit 48 kHz 60 s (ffmpeg truehd encoder, raw) | local 12-core | 1 | 1.04 | 0.3067 s | 196x | bit-exact: 69120000 bytes, md5 6fb81f90 = ffmpeg `-f s32le`; lossless self-check failures 0 |
| 2026-10-18 | ec-mp4 demux | H.264 1080p30 + AAC 128k stereo 60 s, 31.07 MB, 4614 packets | local 12-core | 1 | 1.04 | 0.0018 s | 2.57 M packets/s, 17239 MB/s payload | packets 4614 = ffprobe (1800 video + 2814 audio); payload md5 = ffmpeg `-c copy -f data` for both tracks |
| 2026-10-18 | ec-mp4 mux | same 4614 packets remuxed to a new MP4 (31.07 MB) | local 12-core | 1 | 1.04 | 0.0143 s | 323k packets/s, 2171 MB/s payload | ffmpeg `-c copy -f data` of our output: md5 = source payload, both tracks |

Notes:
- ec-aac HE-AAC fixture: the local ffmpeg's libfdk_aac cannot encode HE-AAC (`-profile:a aac_he`
  fails with "Unable to set the AOT 5: Invalid config" at 16, 22.05, 24, 32, 44.1 and 48 kHz and at
  bitrates 24 to 96 kb/s, mono or stereo, and `aac_he_v2`). The HE-AAC stream is therefore the repo
  fixture `crates/ec-aac/tests/data/heaac_44100_48k_implicit_2s.aac` (HE-AAC v1, stereo, implicit
  SBR, 2.14 s), concatenated 28 times to about 60 s. The decoded length is 54.95 s (the fixture
  decodes to 46 frames of 2048 samples per 2.14 s). Realtime uses the decoded length.
- ec-aac encode quality (lane aacencnoise, 2026-10-18): the 128k output before the lane decoded in
  ffmpeg at correlation 0.9748 with the source (SNR 12.3 dB). The least-squares output gain was
  0.913 per channel (RMS 1.067 times the source's), not 1.04. The lane's MSE-blended allowed noise
  (`MSE_BLEND` in `crates/ec-aac/src/encode.rs`) brings 128k to 0.99794 (SNR 23.9 dB, LS gain 0.997) and
  192k to 0.99896 (SNR 26.9 dB, LS gain 1.000). With `MSE_BLEND = 0.3` the encode is 0.7955 s
  (75x) at 128k and 0.8446 s (71x) at 192k, local load 0.85. The old encoder ran 0.8854 s (68x)
  at load 5.3, so the timings are load-dependent and not a clean comparison. ffmpeg's own encoders on the same source and check: native aac 128k 0.999845, libfdk_aac
  128k 0.999937.
- ec-opus decode: our output keeps the 312-sample pre-skip (ffmpeg trims it), so the correlation is
  taken at lag 312. The sample values match ffmpeg.
- ec-mp4 demux and mux throughput is memory-bandwidth bound: the 31 MB file is in the page cache
  (the 1.8 ms demux reads it from RAM), so the MB/s and packets/s figures are upper bounds for a
  cold file, not disk throughput. The mux writes to the page cache too.
- Speed of the Opus 5.1 decode test on server host c (`decode_speed`, 101x against a 104x floor) is
  a host effect, see `lanes/benchaudio.report.md`. It is not a local number.
- Clips and binaries: `~/.cache/benchaudio-clips` and `~/.cache/benchaudio-bin`. Outputs in
  `~/.cache/benchaudio-clips/out`.

## lane benchhw: local VA-API decode and encode rows (ec-hw)

Local machine with the AMD Radeon RX 9060 XT (radeonsi, gfx1200, Mesa 26.2.4, VA-API 1.23), the
DONE.md D7 exception (gate G6). Each row is the best of 5 timed runs in one process
(`crates/ec-hw/examples/bench.rs`, release build, `cargo build --release -j6`). Every row starts
only when the 1-minute load is below 3; the load at the start of each row is in its column. The
run is `scripts/bench-benchhw.sh`, which generates the clips with the local ffmpeg 8.1.3 and
writes these rows. The clips are synthetic and live outside the repo in `~/.cache/benchhw-clips`.
Code under test: `1cd59b57` plus this lane's example and script.

Clips (all 60 s, 1920x1080, 30 fps, GOP 30): `testsrc2` plus `noise` (alls 10, or 20 for 10-bit).
H.264 High is libx264 CRF 23; HEVC Main and Main10 are libx265 CRF 23; VP9 profile 0 is libvpx-vp9
CRF 31; AV1 8-bit is SVT-AV1 preset 8, CRF 35. The encode source is the raw 8-bit 4:2:0 file of the
same `testsrc2` plus noise, 1800 pictures.

Decode: the timed loop calls `Decoder::decode`, drains `next_frame`, and waits for the GPU on every
picture (`Surface::sync`) before dropping it, so the time is GPU work, not submission. Readback is
not timed. Encode: only the `Encoder::encode` calls are timed; the raw read is outside the timer.
Realtime x = 60 s / best time.

| date | crate | clip | host | threads | load 1m at start | best of 5 | realtime x / rate | check |
|---|---|---|---|---|---|---|---|---|
| 2026-10-18 | ec-hw decode | H.264 High 1080p30 60 s, 1800 pictures, 274.8 MB (36.6 Mb/s) | local, VA-API GPU | GPU | 2.76 | 6.9056 s | 8.69x, 260.66 fps | 1800 of 1800 pictures md5 = ffmpeg 8.1.3 software decode |
| 2026-10-18 | ec-hw decode | HEVC Main 1080p30 60 s, 1800 pictures, 178.9 MB (23.8 Mb/s) | local, VA-API GPU | GPU | 1.96 | 5.0060 s | 11.99x, 359.57 fps | 1800 of 1800 pictures md5 = ffmpeg 8.1.3 software decode |
| 2026-10-18 | ec-hw decode | HEVC Main10 1080p30 60 s, 1800 pictures, 756.7 MB (100.9 Mb/s) | local, VA-API GPU | GPU | 2.15 | 19.3634 s | 3.10x, 92.96 fps | 1800 of 1800 pictures md5 = ffmpeg 8.1.3 software decode (16-bit samples) |
| 2026-10-18 | ec-hw decode | VP9 profile 0 1080p30 60 s, 1800 pictures, 418.7 MB (55.8 Mb/s) | local, VA-API GPU | GPU | 1.18 | 10.0212 s | 5.99x, 179.62 fps | 1800 of 1800 pictures md5 = ffmpeg 8.1.3 software decode |
| 2026-10-18 | ec-hw decode | AV1 8-bit 1080p30 60 s, 1800 pictures, 55.1 MB (7.3 Mb/s) | local, VA-API GPU | GPU | 0.59 | 3.5597 s | 16.86x, 505.65 fps | 1800 of 1800 pictures md5 = ffmpeg 8.1.3 software decode |
| 2026-10-18 | ec-hw encode | H.264 High 1080p30 CBR 8 Mb/s, GOP 30, 60 s | local, VA-API GPU | GPU | 0.60 | 3.9423 s | 15.22x, 456.59 fps, 8006.1 kb/s | ffmpeg decodes all 1800 pictures without error; PSNR vs source 22.79 dB (libx264 at 8 Mb/s, same source: 22.81 dB) |
| 2026-10-18 | ec-hw encode | HEVC Main 1080p30 CBR 8 Mb/s, GOP 30, 60 s | local, VA-API GPU | GPU | 2.34 | 3.4146 s | 17.57x, 527.15 fps, 8457.2 kb/s | ffmpeg decodes all 1800 pictures without error; PSNR vs source 22.80 dB (libx265 at 8 Mb/s, same source: 22.83 dB) |

Notes:
- Decode output check: every picture of the 60 s clip is compared as a whole plane set. For each
  of the 1800 pictures the md5 of our planar I420 (8-bit bytes, or 16-bit little-endian samples for
  Main10) equals the md5 of ffmpeg's software decode of the same file, in display order.
  The check is the bench script CHECK line, over the full set,
  not a sample.
- The encode PSNR is low because the source is heavy noise at 8 Mb/s. The software encoders at the
  same bitrate give the same PSNR (within 0.04 dB), so this is the bitrate and source, not the
  hardware path. The rows measure speed and bitrate; the PSNR is the sanity check that the stream
  carries the picture.
- The Main10 row is slow because its stream is about 100 Mb/s of noise; the realtime figure is the
  decoder on that bitrate, not a Main10 ceiling.
- Not benched here: ec-hw AV1 encode (rows already exist from the av1 lanes, and `av1_encode_speed`
  in `crates/ec-hw/tests/gpu.rs`).
- Clips and binaries: `~/.cache/benchhw-clips` (decode streams, references `ref_*.yuv`, encode source
  `src_1080p30_8bit.yuv`), `~/.cache/benchhw-bin`. Outputs in `~/.cache/benchhw-clips/out`.

## lane benchrest: local still-image, Matroska, FLAC and zlib rows (ec-image, ec-matroska, ec-flac, ec-inflate)

Local 12-core machine, the DONE.md D7 exception (gate G6). Each row is the best of 5 runs in one
process (`examples/bench.rs` in each crate; the 5-run minimum wall time), one thread, built with
`cargo build --release -j6` at `bb0823cc` (no crate source changed; the lane adds the four examples,
the script and this section). Every row started at 1-minute load 1.76 to 1.83 (the coordinator asked
for load1 at or below about 2 while rows are taken; the load at the start of each row is in its
column). The whole run is `scripts/bench-benchrest.sh --no-inputs --no-build` (inputs made by
`scripts/bench-benchrest.sh` with ffmpeg 8.1.3 and python 3 with numpy, in `~/.cache/tmp-benchrest/inputs`,
never committed, no personal media). Realtime x = audio seconds / best time.

| date | crate | input | machine | threads | load1 | best of 5 | realtime x / rate | self-check |
|---|---|---|---|---|---|---|---|---|
| 2026-10-18 | ec-image png decode | PNG RGB8 1920x1080, ffmpeg testsrc2 frame (ffmpeg png encoder) | local 12-core | 1 | 1.80 | 0.0198 s | 104.7 Mpix/s, 314 MB/s out | RGB8 pixels md5 2ef82882 = ffmpeg `-f rawvideo -pix_fmt rgb24`, bit-exact |
| 2026-10-18 | ec-image jpeg decode | JPEG 4:2:0 1920x1080 q2 (ffmpeg mjpeg `-q:v 2`) | local 12-core | 1 | 1.80 | 0.0092 s | 225.4 Mpix/s, 676 MB/s out | vs libjpeg (Pillow) max abs diff 4, PSNR 62.9 dB; vs ffmpeg rgb24 33.25 dB is swscale's conversion (ffmpeg default vs `accurate_rnd+full_chroma_int` differ by up to 142) |
| 2026-10-18 | ec-image png encode | PNG RGBA8 1920x1080 from ffmpeg rawvideo testsrc2 frame (out 288821 bytes) | local 12-core | 1 | 1.80 | 0.0817 s | 25.4 Mpix/s, 101 MB/s in | ffmpeg decode of our PNG md5 a62e1a04 = source RGBA, bit-exact |
| 2026-10-18 | ec-matroska demux | H.264 1080p30 + AAC 128k stereo 60 s mkv (31.04 MB, 4614 packets) | local 12-core | 1 | 1.80 | 0.0026 s | 1.81 M packets/s, 12143 MB/s payload | packets 4614 = ffprobe (1800 video + 2814 audio); payload md5 = ffmpeg `-c copy -f data`, both tracks |
| 2026-10-18 | ec-flac decode | FLAC 16-bit stereo 44.1 kHz 60 s (ffmpeg, level 5) | local 12-core | 1 | 1.80 | 0.0957 s | 627x realtime | PCM md5 49846994 = ffmpeg `-f s16le`, bit-exact (10584000 bytes) |
| 2026-10-18 | ec-flac decode | FLAC 24-bit 5.1 48 kHz 60 s (ffmpeg, s32 container) | local 12-core | 1 | 1.80 | 0.4036 s | 148.7x realtime | PCM md5 9e19ac00 = ffmpeg `-f s32le`, bit-exact (69120000 bytes) |
| 2026-10-18 | ec-flac encode | 16-bit stereo 44.1 kHz 60 s PCM, `EncoderConfig::default()` (export level) | local 12-core | 1 | 1.82 | 0.3961 s | 151.5x realtime, 802 kb/s | ffmpeg decode of our FLAC md5 49846994 = source PCM, bit-exact |
| 2026-10-18 | ec-flac encode | 24-bit 5.1 48 kHz 60 s PCM, `EncoderConfig::default()` (export level) | local 12-core | 1 | 1.82 | 1.5334 s | 39.1x realtime, 4920 kb/s | ffmpeg decode of our FLAC md5 802e8a11 = source PCM, bit-exact |
| 2026-10-18 | ec-inflate zlib | zlib 64 MiB (32 MiB text + 32 MiB 16-bit PCM), python level 6, 41.38 MB in | local 12-core | 1 | 1.76 | 0.2334 s | 287.6 MB/s out | md5 3e7ed26d = python `zlib.decompress`, bit-exact (67108864 bytes) |
| 2026-10-18 | ec-inflate blocks | 20000 zlib streams of 3 KiB text, python level 6 (subtitle-sized), 61.44 MB out | local 12-core | 1 | 1.83 | 0.1923 s | 319.5 MB/s out, 104k streams/s | md5 c5c6044a = python `zlib.decompress` per block, bit-exact (61440000 bytes) |

Notes:
- Export level for ec-flac: `EncoderConfig::default()` (4096-sample blocks, LPC order 12, Rice
  partitions to order 6), the "ec default (edith)" entry of `crates/ec-flac/tests/level_depth_parity.rs`
  (ffmpeg `-compression_level` 5).
- Matroska is checked on payloads and packet counts, the same as the ec-mp4 demux row. The packet
  rate and MB/s are page-cache bound (31 MB file held in RAM): upper bounds, not disk throughput.
- JPEG: the PNG rows are exact. The JPEG output is RGB from our decoder against ffmpeg's yuvj420p to
  rgb24 conversion (swscale chroma upsampling and colour matrix), so the gap is expected to be in
  those steps, not proven. The existing ec-image test compares against the image crate with a
  max delta of 5 (`tests/differential.rs`). Whether ec-image is closer to ffmpeg's RGB is not
  checked here, so no JPEG parity claim is made from this row.
- An earlier run of the same script at load 2.4 to 2.8 gave the same checks and timings within
  about 1% of these. Its rows are discarded (taken above the load limit); only the run at load
  1.76 to 1.83 is recorded.

## 2026-10-23 (doneaudit): DONE audit bench rows

Columns: col 6 is the value as recorded in the audit (unlabelled in the evidence); rates are decode throughput.

| date | crate | clip | host | threads | load 1m at start | fps / rate | realtime x | notes |
|---|---|---|---|---|---|---|---|---|
| 2026-10-23 | ec-prores (decode, main bed76a50) | 1080p 422 HQ 25 fps 24 frames noisy testsrc2 | b (quiet, load 0.05) | 1 | - | 63.7 | 2.55x | set spread 2.7%, median 62.4 |
| 2026-10-23 | ec-prores (decode, main bed76a50) | 1080p 422 HQ 25 fps 24 frames noisy testsrc2 | b (quiet, load 0.05) | 4 | - | 161.1 | 6.44x | `cargo test -p ec-prores --release --test perf` |
| 2026-10-23 | ec-mpeg2 | 1080i29.97 MPEG-2 20 Mb/s IPB 10 s | b (shared) | 1 | 0.47 | 101.5 | 3.39x | md5 = ffmpeg 8.1.3; spread 5-15%, shared host |
| 2026-10-23 | ec-mpeg2 | 576i25 MPEG-2 6 Mb/s IPB 10 s | b (shared) | 1 | 0.73 | 467.5 | 18.70x | md5 = ffmpeg 8.1.3; spread ~8%, shared host |
| 2026-10-23 | ec-mp3 decode (MP2) | MP2 stereo 48 kHz 384 kb/s 60 s (ffmpeg -c:a mp2, pink noise) | b (shared) | 1 | 0.1543 s best | - | 389.0x | max abs 7.3e-7 vs mp2float; spread 4.8% |
| 2026-10-23 | ec-mp3 decode (MP2) | MP2 stereo 48 kHz 192 kb/s 60 s (ffmpeg -c:a mp2, pink noise) | b (shared) | 1 | 0.1496 s best | - | 401.1x | max abs 7.0e-7 vs mp2float; spreads 6-24%, shared host |
| 2026-10-23 | ec-mpegts demux (TsDemuxer) | H.264 1080p30 + AAC 128k + AC-3 5.1 448k 60 s, 51.06 MB, 6489 packets | b (shared) | 1 | 0.16 | 123.8k packets/s, 942 MB/s | - | 0.0524 s best; packets = ffprobe; spread 3.6-17%, shared host |
| 2026-10-23 | ec-mpegts demux (PsDemuxer) | MPEG-2 1080p30 + MP2 224k + AC-3 5.1 448k 60 s, 65.34 MB, 6175 packets | b (shared) | 1 | 0.16 | 61.1k packets/s, 644 MB/s | - | 0.1010 s best; packets = ffprobe; spread 12-28%, shared host |
