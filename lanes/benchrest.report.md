# benchrest: local bench rows for the still-image, Matroska, FLAC and zlib crates (DONE.md gate G6)

## Goal

Add the G6 speed rows (local 12-core machine, DONE.md D7 exception, best of 5, self-check against
ffmpeg or python) for the four crates that had none: ec-image, ec-matroska, ec-flac, ec-inflate.

## Base

- Base sha: `bb0823cc` (main).
- Branch `lane/benchrest`. No crate source changed; the lane adds four dev-only examples, the runner
  script, the bench.md section and this report.

## Changes

- `crates/ec-image/examples/bench.rs`: `png-dec`, `jpeg-dec` (decode, best of N, RGB8 output for the
  check), `png-enc` (encode raw RGBA8, best of N).
- `crates/ec-matroska/examples/bench.rs`: `demux` (all packets, packets/s and MB/s, per-track payload
  dumps).
- `crates/ec-flac/examples/bench.rs`: `dec` (whole buffer, `decode_all` + `to_pcm_bytes`, best of N),
  `enc` (raw s16le or packed s24le to FLAC at `EncoderConfig::default()`, best of N).
- `crates/ec-inflate/examples/bench.rs`: `zlib` (one stream) and `blocks` (length-prefixed streams),
  best of N.
- `scripts/bench-benchrest.sh`: generates every input with ffmpeg 8.1.3 and python 3 (numpy), builds
  the four examples, waits for 1-minute load at most 2 before each row, runs the rows and the checks,
  prints `ROW|` and `CHECK|` lines. Options: `--no-inputs`, `--no-build`. Inputs go to
  `$TMPDIR/inputs` (default `~/.cache/tmp-benchrest/inputs`), never committed.
- `lanes/bench.md`: new section `## lane benchrest` with 10 rows and notes.

No new non-dev dependencies. The examples use dev-dependencies already present in each crate
(ec-image's `image`, ec-inflate's `flate2` are dev-only; the examples reuse the crate's own API).

## Inputs (all generated, none committed, none personal)

- PNG RGB8 1920x1080: `ffmpeg -f lavfi -i testsrc2=size=1920x1080:rate=1 -frames:v 1 -pix_fmt rgb24`.
- JPEG 4:2:0 q2: same frame, `-c:v mjpeg -q:v 2 -pix_fmt yuvj420p`.
- RGBA raw: same frame, `-pix_fmt rgba -f rawvideo`.
- MKV: testsrc2 1920x1080 30 fps + sine 220 Hz, libx264 veryfast 4 Mb/s, AAC 128k stereo, 60 s.
- FLAC 16-bit stereo 44.1 kHz 60 s: tones plus two pink-noise channels (`-sample_fmt s16`).
- FLAC 24-bit 5.1 48 kHz 60 s: six tones plus six pink-noise channels (`-sample_fmt s32 -bits_per_raw_sample 24`).
- zlib 64 MiB: 32 MiB seeded word text plus 32 MiB int16 PCM (numpy), python `zlib.compress(level 6)`.
- zlib blocks: 20000 x 3072 B seeded word text, each python `zlib.compress(level 6)`, `u32le` length prefix.

## Verify commands and results

Timed rows: `scripts/bench-benchrest.sh --no-inputs --no-build` (after one full run with inputs).
Load1 at the start of each row: 1.76 to 1.83 (all valid rows). The first full run was taken at
load1 2.4 to 2.8 (above the limit set by the coordinator); its timings were discarded and re-run at
the lower load. Its checks all passed too (same md5s), so the checks are reproduced in the valid run.

Gates:

- `cargo clippy -p ec-image -p ec-matroska -p ec-flac -p ec-inflate --all-targets -- -D warnings`:
  run on a server (`scripts/remote-run.sh benchrest --host auto`, dev profile, one combined run over the four
  packages). Result: exit 0 (`CLIPPY_EXIT=0`).
- `cargo test --release --no-fail-fast -p ec-image -p ec-matroska -p ec-flac -p ec-inflate`:
  run on a server. Result: exit 0 (`TEST_EXIT=0`), every test binary ok, no FAILED lines.

## Results (local, best of 5, 1 thread)

| crate | input | best | rate | self-check |
|---|---|---|---|---|
| ec-image | PNG RGB8 1920x1080 | 0.0198 s | 104.7 Mpix/s | md5 = ffmpeg rgb24, bit-exact |
| ec-image | JPEG 4:2:0 q2 1920x1080 | 0.0092 s | 225.4 Mpix/s | vs ffmpeg rgb24: max delta 159, PSNR 33.25 dB, 89.4% exact |
| ec-image | PNG RGBA8 1920x1080 encode | 0.0817 s | 25.4 Mpix/s | ffmpeg decode md5 = source, bit-exact |
| ec-matroska | H.264 1080p30 + AAC 60 s demux | 0.0026 s | 1.81 M packets/s | 4614 packets = ffprobe; payload md5 = ffmpeg both tracks |
| ec-flac | 16-bit stereo 44.1k decode | 0.0957 s | 627x realtime | PCM md5 = ffmpeg, bit-exact |
| ec-flac | 24-bit 5.1 48k decode | 0.4036 s | 148.7x realtime | PCM md5 = ffmpeg, bit-exact |
| ec-flac | 16-bit stereo 44.1k encode (default) | 0.3961 s | 151.5x realtime, 802 kb/s | ffmpeg decode md5 = source, bit-exact |
| ec-flac | 24-bit 5.1 48k encode (default) | 1.5334 s | 39.1x realtime, 4920 kb/s | ffmpeg decode md5 = source, bit-exact |
| ec-inflate | zlib 64 MiB mixed | 0.2334 s | 287.6 MB/s out | md5 = python zlib.decompress |
| ec-inflate | 20000 blocks of 3 KiB | 0.1923 s | 319.5 MB/s out | md5 = python zlib.decompress per block |

## Findings

1. ec-image JPEG output does not match ffmpeg's rgb24 conversion bit for bit (PSNR 33.25 dB, max
   delta 159, 89.4% of samples exact). The existing ec-image test compares against the image crate
   (max delta 5), not ffmpeg. The gap is probably chroma upsampling and the colour matrix (swscale vs
   ours), but this lane did not check that. A JPEG-vs-ffmpeg parity lane is needed before any claim.
   Resolved by the orchestrator: on the same file ec-image vs libjpeg (Pillow) is max delta 4,
   62.9 dB, 0 samples off by more than 10. ffmpeg's default rgb24 vs its own
   `bicubic+accurate_rnd+full_chroma_int` rgb24 differ by up to 142 (34.65 dB), so the 33 dB figure
   measures swscale's fast conversion, not the decoder. No decoder gap.
2. The PNG rows are exact against ffmpeg in both directions.
3. The matroska packet rate and MB/s are page-cache bound (31 MB held in RAM): upper bounds only.

## Reproduce

```
scripts/bench-benchrest.sh
```

Binaries: `~/.cache/tmp-benchrest/bin`; build dir `~/.cache/tgt-benchrest`; TMPDIR
`~/.cache/tmp-benchrest`; inputs in `~/.cache/tmp-benchrest/inputs`. `/tmp` was not used by the lane.

## Left open

- JPEG parity with ffmpeg (finding 1).
- Realtime and MB/s rows are local only, per D7; no server timing was taken.
