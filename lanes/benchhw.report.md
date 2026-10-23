# lane benchhw: ec-hw VA-API bench rows (DONE.md gate G6)

Base: `1cd59b57` (main). Branch `lane/benchhw`. Local machine only, with the GPU (AMD Radeon RX
9060 XT, radeonsi, Mesa 26.2.4, VA-API 1.23). The servers have no VA-API, so this lane did not use
`scripts/remote-run.sh`. Build with `CARGO_TARGET_DIR=~/.cache/tgt-benchhw RUSTC_WRAPPER=`, TMPDIR at
`~/.cache/tmp-benchhw`. The fixtures are linked to the primary checkout's `fixtures/`.

## Goal

Speed rows for ec-hw in `lanes/bench.md` (`## lane benchhw`), best of 5, local: VA-API decode for
H.264, HEVC Main, HEVC Main10, VP9 profile 0 and AV1 8-bit at 1080p30 60 s, and VA-API encode for
H.264 and HEVC at 1080p30. Each row carries fps, x realtime and an output check.

## Changes

- `crates/ec-hw/examples/bench.rs` (new): `bench dec` and `bench enc` drive the public `Decoder`
  and `Encoder` API. Decode waits on the GPU per picture inside the timed loop. The check pass
  writes all pictures as I420 for the md5 comparison. Encode times only the `encode` calls.
- `scripts/bench-benchhw.sh` (new): generates the clips with ffmpeg, builds the example, runs the
  rows, and runs the checks, with the load gate (1-minute load below 3 before each row).
- `lanes/bench.md`: new section `## lane benchhw`, with the table and the notes.
- This report.

No crate source changed. `ec-hw` and `ec-va` have no `examples/` before this lane, so the example is
new code only.

## Results (run `~/.cache/benchhw-logs/run2.log`, final)

| row | best of 5 | x realtime | fps | check |
|---|---|---|---|---|
| decode H.264 High | 6.9056 s | 8.69x | 260.66 | 1800/1800 md5 = ffmpeg |
| decode HEVC Main | 5.0060 s | 11.99x | 359.57 | 1800/1800 md5 = ffmpeg |
| decode HEVC Main10 | 19.3634 s | 3.10x | 92.96 | 1800/1800 md5 = ffmpeg (16-bit) |
| decode VP9 profile 0 | 10.0212 s | 5.99x | 179.62 | 1800/1800 md5 = ffmpeg |
| decode AV1 8-bit | 3.5597 s | 16.86x | 505.65 | 1800/1800 md5 = ffmpeg |
| encode H.264 High, 8 Mb/s CBR | 3.9423 s | 15.22x | 456.59 | 8006.1 kb/s, ffmpeg decodes 1800/1800, PSNR 22.79 dB |
| encode HEVC Main, 8 Mb/s CBR | 3.4146 s | 17.57x | 527.15 | 8457.2 kb/s, ffmpeg decodes 1800/1800, PSNR 22.80 dB |

Calibration (same source, same bitrate, ffmpeg software encoders): libx264 PSNR 22.81 dB, libx265
22.83 dB. The HW PSNR is within 0.04 dB of software, so the low PSNR is the noisy source at
8 Mb/s, not the hardware path.

An earlier run (`run1.log`, same clips and binary apart from the calibration line) gave the same
checks and speeds within about 2%: H.264 8.70x, HEVC 11.99x, Main10 3.09x, VP9 5.99x, AV1 16.81x,
encode H.264 15.46x, HEVC 18.00x.

## Verify commands and results

- `vainfo`: radeonsi exposes VLD for H.264 Main/High, HEVC Main/Main10, VP9 Profile0, AV1 Profile0,
  and EncSlice for H.264 Main/High, HEVC Main/Main10 and AV1 Profile0. Only the profiles with
  decode rows above were benched; VP9 profile 2 and AV1 profile 2 were not.
- `cargo clippy -j6 -p ec-hw -p ec-va --all-targets -- -D warnings`: clean, exit 0
  (`~/.cache/benchhw-logs/clippy.log`); re-run clean after rustfmt on the example.
- `EC_REQUIRE_FIXTURES=1 cargo test --release -j6 -p ec-hw`: all pass, exit 0. Unit 19/19, film_grain
  1/1, gpu 41/41 (no "skipped" line in the log, so the GPU tests ran), idr_counting 3/3,
  intra_rate 2/2 (`~/.cache/benchhw-logs/test.log`).
- `scripts/bench-benchhw.sh --no-clips`: all 7 rows and 6 checks, log `~/.cache/benchhw-logs/run2.log`.
  Load at the start of each row was 0.59 to 2.76, below the gate of 3.

## Notes and what is left

- Decode timing waits on the GPU per picture, so the figures are GPU work, not submission. Readback
  is not timed.
- The Main10 row is slow because its stream is about 100 Mb/s of noise, not a decoder ceiling.
- Not done: ec-hw AV1 encode (not requested; `av1_encode_speed` in `tests/gpu.rs` covers it), VP9
  and AV1 profile 2 decode, and bit-depth 10 encode for H.264/HEVC (not a supported profile here).
- Clips are generated with the local ffmpeg 8.1.3 (libx264, libx265, libvpx-vp9, libsvtav1). The
  clip files and reference YUVs (about 45 GB) live in `~/.cache/benchhw-clips`, outside the repo.
- Gate G6 for ec-hw and ec-va: rows met as above. The ec-va gate is the clippy and the existing
  ec-hw GPU tests, since ec-va has no bench row of its own.
