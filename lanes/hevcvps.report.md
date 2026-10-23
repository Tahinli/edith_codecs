# lane hevcvps: conformance package MD5 is the reference for three streams

## Goal

Decide whether our output for VPSSPSPPS_A_MainConcept_1 is correct, using an authoritative reference rather than ffmpeg. Apply the same test to SAODBLK_A and SAODBLK_B, which were open against ffmpeg.

## Base

- Lane branch `lane/hevcvps`, from `main` (merged again after the history rewrite; the merge commit is the lane tip).

## Finding: ffmpeg is not the reference for these three streams

Each conformance package ships a whole-output MD5 file (`<name>_md5.txt`) holding the MD5 of every decoded frame, cropped, in output order. Our decoder output matches it for all three streams. ffmpeg's YUV does not, so ffmpeg is not the reference for them.

| Stream | Package MD5 (authoritative) | ffmpeg YUV MD5 | Ours |
|---|---|---|---|
| VPSSPSPPS_A_MainConcept_1 | `1ddf74263cb4953cfdfcf99c563d88ea` | `7f89d32bec909d34adc41efcb7060627` (2 frames, not 6) | matches package MD5 (6 frames) |
| SAODBLK_A_MainConcept_4 | `b9c98937bcb6be445348fdc89c208f1e` | `23f03fffa670f7d770302c819b04f439` | matches package MD5 |
| SAODBLK_B_MainConcept_4 | `031043495e9b9f4ce120afa316aa5705` | `de53c4f4adb77faca65c994c5fa7e7d0` | matches package MD5 |

- VPSSPSPPS_A: the stream has no picture-hash SEI (zero SEI NAL units), so the SEI check cannot run on it. Its parameter sets are used as the spec describes: 6 SPS and 6 PPS ids, re-sent and re-ordered, each frame activating its own SPS and PPS. ffmpeg logs "PPS id out of range" and outputs only 2 frames; that is ffmpeg's limitation.
- SAODBLK_A and SAODBLK_B: the earlier investigation attributed the mismatch to our SAO. The package MD5 shows our decoder was right; the ffmpeg reference was wrong.

## What changed

- `crates/ec-h265/tests/conformance_intra.rs`:
  - `check_stream` reads `<name>_md5.txt` beside the bitstream when it holds a single digest (`whole_output_digest`). Frames with no hash SEI are hashed into one MD5 over the cropped output and compared to it. This takes precedence over the ffmpeg reference YUV.
  - `KNOWN_INEXACT` is now an empty list. The mechanism (every accepted stream must be exact unless listed) is kept.
  - Unit test `whole_output_digest_reads_the_first_word` added.
- `crates/ec-h265-syntax/src/lib.rs`: `mod md5` is now `pub mod md5` so the test can use `Md5`. `md5.rs` module doc updated to match.
- No decoder change was needed.

## Verify

Run via `scripts/remote-run.sh hevcvps --host a` on the merged tree, with `EC_H265_REF_YUV_DIR=$HOME/refyuv/hevcinter` (the directory that holds the reference YUVs). Results are below. The earlier pre-merge run used `~/refyuv`, which is empty on host b; it is superseded.

| Command | Result |
|---|---|
| `cargo clippy -p ec-h265 -p ec-h265-syntax --all-targets -- -D warnings` | pass (exit 0) |
| `cargo test -p ec-h265 -p ec-h265-syntax --release` | pass, all binaries 0 failed (ec_h265 lib 82; conformance 15; conformance_intra 2; cu_syntax 4; dec_meta 4; dec_shell 13; decode 5; encode 4; perf 3; rdoq_cache_hash 3; ec-h265-syntax 29, fuzz 2, sei_sweep 1) |

Conformance total: `total: 9203 pictures checked, 9203 exact, 147/147 streams accepted`. Every hashless stream was checked against its reference YUV in `~/refyuv/hevcinter`, and the three streams above against their package whole-output MD5.

## Left

- Nothing open for these three streams.
- The ffmpeg reference YUVs in `~/refyuv/hevcinter` are still used for the hashless streams that have no whole-output MD5 file. Their agreement with the package MD5s was not re-checked in this lane.
