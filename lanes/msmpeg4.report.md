# lane/msmpeg4 report

## Works
- MS-MPEG4 v2 (MP42/DIV2/MP41) and v3 (DIV3/MP43/DIV4/DIV5/DIV6/AP41/COL1/COL0/3IVD) decode in ec-mpeg4, bit-exact vs ffmpeg 8.1.3 (`-threads 1 -cpuflags 0`): v3 18 variants, v2 7 variants, 0 mismatched frames (qscale 2..31, all-intra, odd sizes, up to 640x360). New CodecIds MsMpeg4V1/V2/V3; FourCCs mapped in ec-riff.
- DV type-1 AVI (`iavs`): DV video (CodecId::DvVideo) + PCM S16LE audio via ec-dv. PAL video+audio byte-identical to ffmpeg; NTSC audio checked.
- Raw video in AVI (new CodecId::RawVideo): BI_RGB 24/32 bottom-up/top-down, YUY2, UYVY, I420/IYUV, YV12. 8 cases x 50 frames, 0 diff vs ffmpeg rawvideo.

## Refusals
- MS-MPEG4 v1 (MPG4/DIV1): refused (no ffmpeg encoder to verify against).
- Raw 8/15/16 bpp, palettised DIB, YVYU, V210, NV12, Y800: refused.
- MJPEG-B (mjpb in mov): not done, listed as refusal.
- 3IV2, SEDG: refused as before.

## Gate
- `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `EC_REQUIRE_FIXTURES=1 cargo test -p ec-mpeg4 -p ec-riff -p ec-dv -p ec-core`: all pass (registry count 41).
