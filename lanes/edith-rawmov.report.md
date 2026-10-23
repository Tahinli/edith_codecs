# lane-rawmov report

Commits (local, branch lane-rawmov): 4062d253 (avi.rs refused-raw case now 1 bpp, standalone) and the feature commit after it.

## What changed
- demux.rs: mov tracks `raw `, 2vuy, yuvs, yuv2, v210, v410, b16g map to Codec::Raw (video tracks only; PCM audio entries of the same fourcc are skipped because ec-mp4 gives them audio params). `mjpb` maps to Codec::Mjpeg, so it runs on the existing MJPEG seat. Mp4Demuxer keeps the synthesized header (MOV_MARKER) and hands it out via video_params; raw samples pass through whole. A mov raw layout the decoder refuses (e.g. 8-bit palette `raw `) fails the open with a named message plus the ffmpeg hint.
- decode/raw.rs: Rgb8 -> full-range BT.709 4:4:4 like Bgr8; Nv12 -> I420; Gray16 -> I010 (rounded >>6, neutral chroma); Gray8 via the shared fold; I210/I410 carried as is.
- decode/intra.rs probe: chroma/bit depth/tags per output (v210/v410/b16g = 10-bit; gray = Chroma::Gray full range).
- decode.rs: Pulled::next_wide for Raw.
- video_profile::raw: names for mov entries and the new AVI layouts (NV12, YVYU, Y800, v210, RGB555, PAL8, UYVY, ...).
- export.rs (one hunk, SwDecoder::open, for deep10 to merge): the `wide:` condition now also accepts `Codec::Raw` when `demuxer.bit_depth() >= 10`, so 10-bit raw sources export at 10 bits (AV1/HEVC with video_bit_depth None).

## Tests
- crates/engine/tests/rawmov.rs: 17 cases (mov: uyvy, uyvy odd, yuvs, rgb24, rgb24 odd, argb, rgb555, v210, v210 odd, v410, b16g; avi: nv12, nv12 odd, gray, rgb555, pal8, v210) check probe facts, every picture vs ffmpeg by PSNR, seeks/ranges == sequential, MP4 export frame count, and for 10-bit sources a 10-bit AV1 export (pix_fmt yuv420p10le, PSNR vs ffmpeg). Plus a named-refusal test (mov pal8, avi 1 bpp).
- Not done: engine MJPEG-B test. The fixture builder in edith_codecs' ec-mjpeg tests is a hand-built mov writer; I did not port it. The mapping is in place but untested end to end here.
- Odd sizes: the engine draws even sizes (203x151 reports 202x150), as before.

## Gate
Full `cargo test --workspace --release --no-fail-fast` on a server: 3720 passed, 4 failed: the two known hw_midgop tests plus two of mine (the `every_codec_is_reachable` assertion on `mjpb`, and the refusal-test wording), both fixed afterwards and re-run green (`rawmov` 3/3, `every_codec` ok). Clippy (`--workspace --all-targets -D warnings`) found two `chunks_exact` lints in raw.rs, fixed, then clean on engine. The last two fixes were re-run per target, not as one full-workspace rerun.
