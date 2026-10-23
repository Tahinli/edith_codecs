# lane-oldavi report

Branch lane-oldavi (from 234bd267). Local commits only, not pushed.

## Commits
- 9f2ff367 feat(engine): MS-MPEG4 v2/v3, raw video and DV type-1 (iavs) in AVI
- 7bb6d02b chore: Cargo.lock catch-up (ec-riff now depends on ec-dv)

## What works
- MS-MPEG4 v2/v3 (MP42/DIV2, DIV3/MP43...): `Codec::MsMpeg4`, decoded by the ec-mpeg4 seat. Probe profile "v2"/"v3".
- Raw video in AVI: `Codec::Raw`, new seat `decode/raw.rs` on the intra-only stream.
  BGR24/BGRA become full-range BT.709 4:4:4 (tagged as such); YUY2/UYVY/I420/YV12 keep their chroma. The bottom-up flip is done by ec-riff.
  Probe profile: BGR24, BGRA, YUY2, UYVY, I420, YV12.
- DV type-1 (iavs): the AviSrc adapter now lists the PCM streams ec-riff splits out of each frame, so the sound opens and plays.
- Seeks, ranges and export work for all of them.
- Engine Cargo.toml: no new deps. Cargo.lock: ec-riff -> ec-dv only.

## Files
- crates/engine/src: demux.rs, demux/avi.rs, demux/ts.rs, decode.rs, decode/intra.rs (Seat::open now takes the header; probe takes the header), decode/mpeg4.rs, decode/raw.rs (new), decode/{dv,dnxhd,prores,mjpeg}.rs (signature only), colorspace.rs, video_profile.rs
- crates/engine-hw/src/lib.rs (new Codec variants take no VA-API path)
- crates/engine/tests/oldavi.rs (new), crates/engine/tests/avi.rs (refusal test updated), crates/mcp/src/tools/media.rs (codec_phrase test lines)
- crates/app untouched.

## Tests (oldavi.rs, synthetic ffmpeg media)
- Cases: msmpeg4 DIV3, MP43, msmpeg4v2; rawvideo bgr24, bgra, yuyv, uyvy, i420, yv12 (the last by FourCC YV12); DV type-1 PAL and NTSC built from a .dv.
- Frames are compared to ffmpeg by PSNR (raw RGB 47 dB, YUY2 35 dB, I420 48 dB).
- Seeks and ranges are compared exactly against a sequential decode.
- Type-1 DV frames equal the raw .dv decode, sound equals the raw .dv sound, and the weave is checked against ffmpeg's decode.
- Export frame counts are checked.

## Gate
- `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `cargo test --workspace --exclude engine-hw --release --no-fail-fast` on server c: 3663 passed, 2 failed, 61 ignored.
  The 2 failures are the known h264/hevc_mid_gop_seeks_match_a_sequential_decode (VA-API).
- engine-hw was excluded on the server because host c had no libva at that time. It was covered locally by the coordinator: 7 passed.

## Remaining refusals (named, with the ffmpeg convert line)
- MS-MPEG4 v1 (MPG4/DIV1).
- Raw 8/15/16 bpp, palettised DIB, NV12, YVYU, V210, Y800.
- 3IV2, SEDG and other unknown FourCCs.
- Test: `unreadable_raw_and_msmpeg4_v1_are_refused_by_name_and_the_sound_still_opens` (NV12 and a DIV3 file patched to MPG4).
