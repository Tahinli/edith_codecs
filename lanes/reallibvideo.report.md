# lane reallibvideo: real-library HEVC and AV1 bit-exactness

## Goal
DONE.md criteria:
- "`real_library.toml` HEVC entries, including iPhone and Android phone clips (8-bit and 10-bit HDR), are bit-exact vs ffmpeg" (ec-h265)
- "`real_library.toml` AV1 entries are bit-exact" (ec-av1)

Base: `main` at `fbfe7c9e`. Branch `lane/reallibvideo`.

## Changes
- `crates/ec-h265/tests/real_library.rs` (new): `real_library_hevc_streams_match_ffmpeg`, `#[ignore]`d. Reads every `hevc` row of `fixtures/real-library-manifest.tsv`, extracts the first 300 pictures to Annex B with ffmpeg (`hevc_mp4toannexb`), decodes NAL by NAL with `ec_h265::dec::Decoder`, crops to the conformance window, and compares each frame with `ffmpeg -c:v hevc -pix_fmt <manifest pix_fmt>`. Out-of-scope pixel formats (anything other than yuv420p / yuv420p10le) are refused by name before decoding. A decoder error containing "unsupported" is also a refusal. Mismatches report frame, plane, x, y.
- `crates/ec-av1/tests/real_library.rs` (new): `real_library_av1_streams_match_ffmpeg`, `#[ignore]`d. Same shape: extracts 300 shown frames as an OBU stream, decodes with `ec_av1::stream::decode_stream_with` (film grain applied as the decoder outputs it), and compares with `ffmpeg -c:v libdav1d`.
- No decoder source changed. No mismatch was found, so there was nothing to fix.
- Fixtures: `fixtures/real-library-manifest.tsv` (gitignored) was regenerated with `scripts/scan-real-library.sh ~/Videos ~/Downloads ~/Music`. The old one was stale and lacked 4 Ocean's Eleven HEVC files. The regenerated manifest has 190 rows (was 143). It is shared through the primary checkout, so the H.264 `real_library_streams_match_ffmpeg` input also changed. The script excludes `*.export*.mp4` by design, so `~/Music/reell.export.mp4` (HEVC) is not swept.

## Verify commands and results
Run locally (the personal-media exception), with `CARGO_TARGET_DIR=~/.cache/tgt-reallibvideo TMPDIR=~/.cache/tmp-reallibvideo EC_H265_THREADS=6`, one job at a time:

- `cargo test --release -p ec-h265 --test real_library -- --ignored --nocapture`: 26 bit-exact, 0 refused, 0 failed (81 s).
- `cargo test --release -p ec-av1 --test real_library -- --ignored --nocapture`: 2 bit-exact, 0 refused, 0 failed (18 s).
- Negative check: a one-byte flip injected into AV1 frame 5 was reported as `MISMATCH 1 of 300 frames; first: frame 5 plane Y x=50 y=0` in both files. The injection was reverted.
- `cargo clippy -p ec-h265 -p ec-av1 --test real_library -- -D warnings`: clean. `rustfmt --edition 2024` applied to both new files.
- Not run: the conformance suites (`conformance_intra` on host a, AV1 `cargo test` on b/c). No source changed, so the lane's rule for re-running them after a fix does not apply.

## Per-file table

HEVC (ec-h265), 300 frames each, all bit-exact:

| file (truncated) | container | resolution | bit depth | frames | result |
|---|---|---|---|---|---|
| reel-f6405d | mkv | 3840x2160 | 10 | 300 | bit-exact |
| reel-288f84 | mkv | 1920x1080 | 10 | 300 | bit-exact |
| reel-a354b8 | mkv | 1920x1080 | 8 | 300 | bit-exact |
| reel-201c4a | mkv | 1920x1080 | 8 | 300 | bit-exact |
| reel-a0f392 | mkv | 1920x1080 | 8 | 300 | bit-exact |
| reel-d3e648 | mkv | 1920x1080 | 8 | 300 | bit-exact |
| reel-045982 | mkv | 3840x1608 | 10 | 300 | bit-exact |
| reel-a1717a | mkv | 3840x1600 | 10 | 300 | bit-exact |
| reel-73b372 | mkv | 1920x1038 | 10 | 300 | bit-exact |
| reel-74e84c | mkv | 1920x1080 | 10 | 300 | bit-exact |
| reel-159f5e | mkv | 1920x1080 | 10 | 300 each | bit-exact |
| reel-fa1984 | mkv | 1920x800 | 10 | 300 | bit-exact |
| reel-e30332 | mkv | 712x480 | 10 | 300 | bit-exact |
| reel-25b08a | mkv | 720x480 | 10 | 300 | bit-exact |
| reel-9ec56a | mkv | 720x480 | 10 | 300 | bit-exact |

AV1 (ec-av1), 300 frames each, all bit-exact:

| file (truncated) | container | resolution | bit depth | frames | result |
|---|---|---|---|---|---|
| reel-ca1d65 | mkv | 1920x792 | 10 | 300 | bit-exact |
| reel-04a2c5 | mkv | 3840x1608 | 10 | 300 | bit-exact |

Refused by name: none. The library has no 4:2:2, 4:4:4, 12-bit, RExt or AV1 profile 1/2 streams, so the refusal paths have not been exercised on real files. They are covered by the manifest pixel-format check and the decoder's "unsupported" errors.

## Phone clips
No iPhone or Android HEVC/AV1 clip exists on this machine. Checked: every `.mov`, `.mp4`, `.m4v`, `.3gp` and `.heic` under `~/Videos`, `~/Downloads`, `~/Pictures`, `~/Desktop` and `~/Music`, ffprobed for HEVC/AV1 video. The only camera-style clips are H.264: `~/Downloads/VID_20261003_021934.mp4` (Android, `com.android.version=16`, H.264 High 1280x720) and `~/Desktop/a/asd/late.mov` (H.264 1920x1080, DaVinci Resolve export). Neither is HEVC or AV1, so neither is swept. No stand-in clip was generated. A generated x265 10-bit HDR clip is not added in this lane, because it does not replace phone footage.

## Left over
- DONE.md's "iPhone and Android phone clips (8-bit and 10-bit HDR)" criterion is not met: no such clips exist on this machine. It needs real HEVC footage from a phone, then rerunning `real_library_hevc_streams_match_ffmpeg`.
- The refusal paths (4:2:2/4:4:4, 12-bit, RExt, AV1 profile 1/2) have no real-file coverage here.
- No mismatches were found, so there is no frame/block triage to report.
