# lane hevcencrcpar: ec-h265 1080p30 bitrate-controlled encode, GOP-parallel

Goal: 1080p30 encode under bitrate rate control reaches >= 30 fps at 6 threads, local,
4000 kb/s. Base: main 8fb9fb6c (serial GOPs under rate control, about 28 to 37 fps on the runs
here depending on load).

## Result

Met. 51.9 and 51.4 fps at 6 threads (best of 5, two interleaved rounds), against 36.9 and 36.6
fps for main built from the same tree at the same time (1.73x realtime). 1 thread is unchanged
(13.97 vs 14.21 fps, one round, within noise).

## Approach: option (b), with batch feedback

Rate control makes GOP k depend on the bits of GOPs before it, so GOPs cannot simply be coded in
parallel. Option (b) splits the work so they can be:

- GOPs run in batches. The first GOP is coded alone. Each later batch is 6 GOPs
  (`GOP_RC_BATCH`), spread over `threads` lanes; the batch size is fixed so output does not depend
  on the thread count.
- Each GOP starts a fresh `BitRateControl` seeded with the complexity model and last QPs of the
  last GOP of the previous batch. Its bucket starts empty, since it cannot see the other GOPs'
  bits.
- A batch-level QP offset carries the rate error. After each batch,
  `offset += 0.7 * 6 * log2(bits / budget)`, clamped to +-12, and that offset shifts every
  picture of the next batch.

Measured without the warm-up GOP and the offset, GOPs started cold and a 1000 kb/s test was 10%
high (`tests/rate_control.rs`). With them the rate test is within 5% at 1, 2 and 4 threads.

Changes:
- `crates/ec-h265/src/enc_p.rs`: `encode_sequence_with` sends bitrate sequences (keyint 2..64)
  through `encode_gops_parallel` at every thread count (1 included). `run_sequence` takes the rate
  model by `&mut` so the batch can carry it. `GopMode` selects constant QP or bitrate. Constant-QP
  batching is unchanged.
- `crates/ec-h265/src/enc_rc.rs`: `qp_offset`, `continue_from`, `offset_after_batch`.
- `scripts/hevc-thread-bench.sh`: `RC_KBPS` (and `FPS`) switch the bench to bitrate mode through
  `hevc_rc`. The byte-identity check across thread counts still applies.
- `scripts/hevc-rc-bdrate.sh` (new): bitrate counterpart of `hevc-bdrate.sh`, ec-h265 at fixed
  kb/s points against x265 `-preset veryfast -b:v` with bframes=0.

The output bytes change in bitrate mode (they are not the serial model's bytes any more), but
they are the same at every thread count. Constant-QP output is unchanged.

## Speed (local, 1920x1080 testsrc2 scroll, 180 frames, 4000 kb/s, GOP 30, best of 5)

| threads | main fps | new fps | load avg 1 min (new/main at round start) |
|---|---|---|---|
| 1 | 14.21 | 13.97 | 0.10 / 1.03 (round 1) |
| 6, round 1 | 36.87 | 51.86 | 0.10 / 1.03 |
| 6, round 2 | 36.62 | 51.37 | 2.26 / 2.78 |

The bench time includes writing the recon YUV that `hevc_rc` produces, on both sides.
Streams: 2978622 bytes (new) and 2996888 (main), identical at 1 and 6 threads on each.

## Quality and rate

- `scripts/hevc-rc.sh` (host c, 60 s 1280x720 testsrc2, keyint 30), before the rustfmt-only change:

  | target kb/s | achieved | error | psnr_y | ec decode | ffmpeg decode |
  |---|---|---|---|---|---|
  | 1000 | 998.0 | -0.20% | 37.69 | match | match |
  | 2500 | 2501.0 | +0.04% | 41.17 | match | match |
  | 5000 | 4992.8 | -0.14% | 46.32 | match | match |

- BD-rate against x265 veryfast in bitrate mode (`scripts/hevc-rc-bdrate.sh`, same clip, points
  1000, 2000, 4000, 8000 kb/s, GOP 30, host c):
  - new: YUV +21.16% (Y +16.09%).
  - main: YUV +22.69% (Y +17.46%).
  Both within the +25% limit. The new output is slightly better than main's serial model on this
  clip, but both are about 1 to 1.5 dB below x265 at the same rate.
- Constant-QP BD-rate is unchanged, since the constant-QP bytes are unchanged: +13.0 / +16.8 / +18.8
  YUV (352x288 / 720p testsrc2 / 720p natural) from hevcencspeed4.

## Verify

- Constant QP byte-identical to main: 1920x1080, 30 frames, QP 30, GOP 8, threads 1 and 6 give
  629083 bytes with md5 d873580b761a2f55d3e3787206fd5e30 on both builds (local bench).
- `cargo clippy -p ec-h265 --all-targets -- -D warnings`, host b, after the final edits: exit 0.
- `cargo test --release -p ec-h265`, host b, after the final edits: TESTRC=0, every binary ok
  (`rate_control` 2 passed, lib 89 passed, all others ok).
- `scripts/hevc-gop-roundtrip.sh`, host b: all six rows exact for ec-h265 and ffmpeg decode.
  Run before the rustfmt-only change; that change is whitespace only.
- Conformance, host a, `EC_H265_REF_YUV_DIR=$HOME/refyuv/hevcinter cargo test --release -p ec-h265
  --test conformance_intra`: `9203 pictures checked, 9203 exact, 147/147 streams accepted`, both
  before and after the rustfmt-only change.
- `scripts/hevc-rc.sh` on host c: table above, before the rustfmt-only change.
- No source file over 5000 lines (enc_p.rs 2396).
- Bench harness fix on the way: the bench script's `fps` variable is overwritten by the measured fps
  after each thread count, so the bitrate mode now uses `rc_fps`. The first attempt wrongly showed a
  byte mismatch from that.

## What is left

- The first GOP of each sequence is coded alone (at 6 workers), which costs about one GOP of
  serial-ish time. A 60 s clip is 60 GOPs, so this is a small share; short clips lose more.
- Bitrate output differs from the serial model. The batch feedback corrects rate on a one-batch
  delay (6 GOPs, 6 s at 30 fps at keyint 30), so the rate lands within about 0.2%, not as tightly as
  the per-picture bucket would.
- Batching holds up to 6 GOPs of frames and their output in memory (about 0.6 GB of 1080p
  input at keyint 30).
- Option (a), overlapping picture N+1 with the tail of N, was not tried; picture N+1's QP depends on
  picture N's bit count, so it would also need the offset approach.
