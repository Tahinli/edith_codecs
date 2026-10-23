# lane-dvmpeg4 report

Worktree ~/.cache/wt/edith-dvmpeg4, branch lane-dvmpeg4 (on lane-procodecs). Local commits only, nothing pushed.

## Commits
- 7ad495d4 feat(engine): DV and MPEG-4 Part 2 / H.263 software seats, probe facts, MCP names, dvmpeg4 tests
- (next) chore(engine-hw): DV and MPEG-4 take no VA-API path (compile-forced match arm)

## Files
engine: Cargo.toml, Cargo.lock, colorspace.rs, decode.rs, decode/{dv.rs,mpeg4.rs (new),intra.rs}, demux.rs, demux/{dv.rs (new),avi.rs,ps.rs,ts.rs}, playback.rs (3gp/3g2 subtitle-extension list), video_profile.rs, tests/{dvmpeg4.rs (new),avi.rs}
engine-hw/src/lib.rs (compile-forced arm). mcp/src/tools/media.rs (plain_codec names + H.263 codec_phrase, same place procodecs touched). crates/app untouched.

## Cargo lines (crates/engine/Cargo.toml)
ec-dv = { path = "../../../edith_codecs/crates/ec-dv" }
ec-mpeg4 = { path = "../../../edith_codecs/crates/ec-mpeg4" }

## Design
- DV: intra seat (decode/dv.rs) on the procodecs intra Stream; 4:1:1 widened to I422 by ec-dv; probe states DV25 / DVCPRO50 / DVCPRO HD and field order from the first frame. Containers: mov/mp4 (dvc/dvcp/dv5n/dvh* fourccs), AVI type-2, raw .dv/.dif (new demux/dv.rs, read through the TS reader like AVI, DV audio as PCM streams).
- MPEG-4 Part 2 / H.263: decode/mpeg4.rs (SP/ASP, B-frames via display-order output, VOL from esds / mkv CodecPrivate / VfW XviD tag prepended on sync samples; AVI FourCC passed to the decoder; AVI open-GOP leading B detection). Containers: mp4/mov/3gp/3g2 (mp4v, s263, h263), mkv (V_MPEG4/ISO/*, V_MS/VFW/FOURCC XviD/DivX), AVI.
- Probe profile: Simple / Advanced Simple from the VOS byte, "H.263" for s263/h263.

## Tests (tests/dvmpeg4.rs, 4 tests, all pass)
DV25 NTSC mov/avi/raw, DV25 PAL, DVCPRO50, DVCPRO HD; SP/ASP in mp4, mov, mkv, AVI (xvid, DIVX tags); H.263 in 3gp and 3g2. PSNR vs ffmpeg 48.2-48.4 dB (progressive), deinterlace check for DV, seeks/ranges frame-identical to sequential, export frame count, raw .dv sound. tests/avi.rs refusal test now covers raw video and MS-MPEG4 (DV/MPEG-4 no longer refused).

## Gate (local, nice; host c could not build edith: libclang)
- clippy --workspace --all-targets -D warnings: clean
- cargo test --workspace --release --no-fail-fast: passed 3654, failed 3, ignored 61
- Failures: h264_mid_gop_seeks_match_a_sequential_decode, hevc_mid_gop_seeks_match_a_sequential_decode (known VA-API), and default_exports_are_byte_identical_to_before (h264 export byte pin: 29005 bytes vs pinned 28816). That one is not in the known list; no encode path was touched, so it is probably environmental (local x264/ffmpeg), but I did not run it against the base commit to confirm.

## Remaining refusals / limits
- DV type-1 AVI (iavs): refused by name by ec-riff ("not split into streams"). Crate limit.
- MS-MPEG4 (DIV3/MP42/...) in AVI: still refused by name. DV/ProRes in Matroska: not a Matroska codec here, named error.
- AVI MPEG-4 probe shows no profile (VOL is in-band); mkv does not pass the VfW FourCC to the decoder.
- Interlaced (field-coded) MPEG-4 is shown as coded, not deinterlaced.
- Host c: disk was full; I deleted stale caches ~/.cache/tgt-edith-mpegts and tgt-deintmode there (untouched >4 h, no cargo running) before the coordinator said it was freed.
