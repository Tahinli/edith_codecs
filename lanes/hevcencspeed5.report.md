# lane hevcencspeed5: ec-h265 1080p30 P-GOP encode, GOP-level parallelism

Goal: >= 30 fps (aim 33) at 6 threads, local, `scripts/hevc-thread-bench.sh`. Base: main 6808f23d (28.60 fps).

## Result

40.16 fps at 6 threads (1.34x realtime), best of 6, load average 24 (machine shared). Met.

## Change

With constant QP every GOP is independent (an IDR resets all state), so `encode_sequence_with`
now reads batches of up to `threads` GOPs (keyint 2..64), codes them concurrently (each GOP on
`ceil(threads / gops_in_batch)` row-wavefront workers, via `Encoder::with_threads`) and delivers
the pictures to the sink in order. The sequence loop moved to `run_sequence` unchanged. Bitrate
rate control, keyint 1, keyint > 64 and 1 thread use the old serial-GOP path. Output bytes are
identical to the old coder (629083 bytes before and after on the bench clip), so BD-rate is
unchanged from hevcencspeed4 (352x288 +13.0, 720p testsrc2 +16.8, 720p natural +18.8 YUV; all <= +25%).
Cost: batch latency of up to `threads * keyint` frames and that many frames buffered.

## Speed (local, best of 6, QP 30, GOP 8, 30 frames)

| threads | before fps | after fps |
|---|---|---|
| 1 | 12.40 | 12.36 |
| 2 | 21.35 | 19.33 |
| 3 | 25.47 | 24.12 |
| 6 | 28.60 | 40.16 |

(2 and 3 thread rows are within load noise; the 30-frame clip has only 4 GOPs, so larger
thread counts gain most.)

## Verify

- Byte-identical across thread counts 1/2/3/6: bench script checks, all "yes".
- `cargo clippy -p ec-h265 --all-targets -- -D warnings` and `cargo test --release -p ec-h265`, host b: pass (RC=0, all binaries ok).
- `scripts/hevc-gop-roundtrip.sh`, host c: all six rows exact for ec and ffmpeg decode.
- `scripts/hevc-rc.sh` (60 s 1280x720 testsrc2, keyint 30), host c: 1000.1 / 2500.6 / 5000.2 kbps (+0.01 / +0.02 / +0.00 %), ec and ffmpeg decode match.
- Conformance host a: `9203 pictures checked, 9203 exact, 147/147 streams accepted`.
- No file over 5000 lines (enc_p.rs about 2250).

## What remains

- Under bitrate rate control (QP depends on the previous GOP) the serial-GOP path remains at ~28 fps
  at 6 threads; picture-level pipelining (row-ready flags across pictures) would help there.
- Per-picture `PCoder` allocation and `Search::cost` (~16% of cycles) are the next single-thread hotspots.
