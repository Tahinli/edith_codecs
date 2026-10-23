# procodecs / avi

Worktree `/home/user/.cache/wt/edith-procodecs-avi`, branch `lane-procodecs-avi`, server lane `edith-procodecs-avi`. Commits: the feature commit, then a clippy-allow commit. No trailers, not pushed.

## Design
Option chosen: reuse the TS/PS readers (least code). New `crates/engine/src/demux/avi.rs` holds `AviSrc`, an adapter over `ec_riff::AviDemuxer` that is a third `Src` variant in `demux/ts.rs` (`Src::Avi`). It rescales every stream's `dwScale/dwRate` time base to the 90 kHz ticks the readers use (and seeks back), synthesises the `ec_mpegts::TrackInfo` list, and derives the codec config the AVI does not state (H.264: `avcC` of the first SPS/PPS from the strf tail or first keyframes; MPEG-2: sequence header + extension). `TsDemuxer` (picture) and `TsAudio` (sound) then serve AVI unchanged except:
- `Flavor {Ts, Ps, Avi}` replaces the `ps: bool` (refusal wording `in.avi`, `Flavor::of(path)`).
- `TsDemuxer::open` maps `Mjpeg/ProRes/Dnxhd` to `Codec::{Mjpeg,ProRes,Dnxhd}`; `pending` is now a queue; `field_order()` exposed (`Demuxer::field_order()` public, for `MjpegDecoder::set_field_order`).
- `is_mpeg_system` includes `is_avi`, so audio.rs (probe_streams, Track::open, seek, waveform, export audio source, sync), subtitle, export copy-guard, `header_streams`, container name (`avi`) all route AVI with no further edits.
- `TsDecoder`: little-endian PCM (u8, s16, s24, s32, f32) and raw AAC with its AudioSpecificConfig.
- Probe: `header_streams` names mjpeg/prores/dnxhd, PCM bit depth, and lists a refused picture by ffprobe name (`rawvideo`, `dvvideo`, `mpeg4`, `msmpeg4v3`, ...).

## B-frames (no PTS in AVI)
Decoders release pictures in display order themselves, so sequential decode is right. A keyframe's display index is slot + the run of B pictures right behind it (open GOP leading Bs), detected from the bitstream (MPEG-2 picture_coding_type, H.264 slice_type); a seek goes back a keyframe if that index passes the target. Verified with x264 (bframes=2, closed and open-gop) and mpeg2video `-bf 2`: every picture equals the stream the AVI was remuxed from; seeks equal sequential.
Limits: a zero-size video chunk (dropped frame) keeps its slot but yields no picture, so later frames drift; a non-B-run open-GOP shape (e.g. pyramids where the leading pictures are not B slices) is taken at its slot; 96 kHz+ PCM sample positions may be one sample off (90 kHz tick).

## Refusals
Raw video, DV, MPEG-4 Part 2 / DivX / XviD / MS-MPEG4, other FourCCs: named, with `convert it with ffmpeg -i in.avi -c:v libx264 -c:a aac out.mp4`, in `Demuxer::open`/`picture_refusal` and (when audio exists) in the probe stream list. A file with a refused picture and usable audio opens as audio (probe_streams decodable, AudioSession::open Some); its picture open fails with the refusal. A video-only refused file: open error names it; probe lists no streams (ec-riff errors out of the whole open).
Note: ffmpeg remuxing an mp4 into AVI writes AVCC-framed H.264 (not Annex B) unless `-bsf:v h264_mp4toannexb`, and a bogus 600 fps rate unless `-r 25`; such files are not supported (tests remux with both).

## Tests (`crates/engine/tests/avi.rs`, 7, all pass)
b-frame pictures (h264, h264 open-gop, mpeg2): probe, 50 frames, pictures exact vs source, seeks and ranges equal sequential; mjpeg+pcm: probe, packets byte-equal to ffmpeg's, seek, field order, pictures PSNR>30 dB vs ffmpeg (SKIPPED with a message until the MJPEG decode seat exists; runs automatically after the merge); sound for pcm/mp3/mp2/aac/ac3 vs ffmpeg + seek == sequential; flash/beep sync after open and seek (h264+mp3, mpeg2+mp2, mjpeg+pcm sound only); export duration/frame count/sync; raw/dv/mpeg4 refusals with and without audio; sniff by bytes and `.divx`.
ProRes/DNxHD in AVI map to their Codec variants (no test; needs the other lane's seats).

## Required edits elsewhere
- export.rs: none. crates/app/subs.rs: none (AVI has no subtitles). playback.rs: none.
- Cargo.toml: none (ec-riff dep already present).
- Merge notes: demux.rs touched at `mod avi`, `is_avi` re-export, `container_name`, `Demuxer::field_order`; the picture-through-the-seat check for MJPEG in my test passes after the merge.

## DV / MPEG-4 Part 2
ec-riff now maps both (DvVideo, Mpeg4Part2); per coordinator decision the engine keeps refusing them by name (`dvvideo`, `mpeg4`) with the ffmpeg line (commit 1c106bd6), tested.

## Gates (host c, commit 1c106bd6)
- clippy `-p engine -p edith_mcp -p edith --release --all-targets -D warnings`: clean.
- full suite: passed 3587, failed 3: the 2 known VA-API-only (h264_/hevc_mid_gop_seeks_match_a_sequential_decode) and `ui::icons::tests::every_icon_is_baked_and_every_file_has_a_variant` (edith app, asset-listing test; not touched by this lane, likely a base/asset-sync issue on the server). tests/avi.rs 7/7 pass.
