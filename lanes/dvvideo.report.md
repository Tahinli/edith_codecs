# lane/dvvideo report

New crate `crates/ec-dv` (DV video + DIF audio + raw .dv demuxer). Branch `lane/dvvideo`, rebased on main (registry count 33, DNxHD present). No push.

## API for edith integration
- `ec_dv::DvDecoder` implements `ec_core::registry::Decoder` for `CodecId::DvVideo` (`decode`, `decode_native`). Output: 4:2:0 -> I420, 4:2:2 -> I422, 4:1:1 -> I422 (see decision). `to_video_frame`, `output_format(Chroma)` are public. Interlace flag/field order come from the VAUX VSC pack (SD mostly bottom-field-first; 1080 flagged; 720p progressive). ColorInfo: HD 1/1/1, PAL 5/6/5, NTSC 6/6/6.
- `DvVideoDecoder::decode(&[u8]) -> DvPicture` gives the native planes (incl. true 4:1:1), `PROFILES`, `frame_profile`.
- `extract_audio(frame) -> Option<DvAudio{sample_rate, samples, nonlinear_12bit, pairs}>` (i16 per stereo pair).
- `DvDemuxer<R>` (Demuxer trait) + `is_dv(head)` / `dv_probe_score`: stream 0 = DvVideo (pts in frames), streams 1.. = `PcmS16Le` stereo pairs. Seek by frame, junk skipping.
- Containers: ec-mp4 maps dvc/dvcp/dvpp/dvcs/dvis/dv5n/dv5p/dv5q/dvhq/dvh1-6/dvhp/dvhs/dvhn/dvhr -> DvVideo; ec-riff AVI maps dvsd/dv25/dv50/dvhd/CDVC/dvh1/dvsl/dvc/dvcs/dvpp -> DvVideo; type-1 DV AVI ('iavs') is a named refusal.
- ec-probe's decoder factory is NOT wired to ec-dv (integration is edith's).

## Decisions
- No I411 PixelFormat in ec-core: the Decoder trait widens 4:1:1 to I422 by exact duplication of each chroma sample; native 4:1:1 via `DvVideoDecoder`/`DvPicture`. Byte-exactness is verified in the native format.
- No audio CodecId: the demuxer emits PCM (PcmS16Le); 12-bit nonlinear is expanded to 16-bit linear like ffmpeg.
- Registry edit is one variant (`DvVideo`) + arms + test count.

## Exactness (vs ffmpeg dvvideo, tests make fixtures with ffmpeg at test time)
9 flavours (ntsc411, pal420, pal411, ntsc422, pal422, hd1080i50/60, hd720p50/60) byte-exact in native pix_fmt; audio sample-exact (16-bit 48/44.1/32k, 12-bit nonlinear; not muxable for 720p). Mutated/damaged streams are also byte-exact vs ffmpeg `-cpuflags 0` (covers retry, concealment, 248 mode, HD last-row field).
Quirks reproduced: ffmpeg VLC consumes the full sub-table depth (15-bit run0/level0 code eats 16 bits); the ffmpeg CLI default `-ec 3` activates the DIF status-nibble check (sta==0xe or differing -> segment damaged); ffmpeg's SIMD IDCT differs from the C simple IDCT only on out-of-range (corrupt) coefficients; ec-dv matches the C one, valid streams are identical.

## Speed
Segments decode on up to 8 threads (std::thread::scope, per-segment write logs applied in order, result identical to serial). On an idle server (host a) the perf test passes (SD >= 300, HD >= 60). On the shared servers under other jobs it measured 134-217 fps SD, 68-76 HD, so `tests/perf.rs` skips the floor when load average > cores/2 (prints fps either way). Single-thread cost is about 49M instructions per DV50 PAL frame (IDCT ~30%, entropy ~26%).

## Fuzz
`crates/ec-dv/fuzz` target `decode_dv` (frame with forged header per profile, raw bytes, audio extraction, demuxer), 5 single-frame seeds committed. 10 min on server: 3741 execs (big inputs), no crash.

## Gates
Workspace `cargo clippy --workspace --all-targets --release -- -D warnings` clean; `EC_REQUIRE_FIXTURES=1 cargo test -p ec-dv -p ec-mp4 -p ec-riff -p ec-core --release` all green except the load-sensitive perf floor (see above; passes on an idle host).
