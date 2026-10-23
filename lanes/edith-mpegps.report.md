# Lane edith-mpegps report

Branch `lane-mpegps` (worktree `/home/user/.cache/wt/edith-mpegps`), one commit rebased onto main `abf85081` (lane-mpegts merged). Gitmoji message, no trailers. Not pushed. No Cargo.toml change.

## What changed
- `crates/engine/src/demux/ps.rs` (new): `is_mpegps(path)` (own extensions `m1v m2v mpv vob m2p tod` trusted; `mpg mpeg mod` and any other name decided by bytes; Matroska/mp4/TS-named files never), `is_mpeg_system` (TS or PS), `carries_picture`, `container_name` (`mpeg`, `mpegvideo`, or `vob/mod/tod/m2p`), `ps_error`. Re-exported from `demux`.
- `demux/ts.rs` factored, TS behaviour unchanged: a `Src` enum (TsDemuxer | PsDemuxer) sits behind `TsDemuxer` (picture), `TsAudio` (sound), `subtitles`, `header_streams`, `describe`. Both are read by the same code, same time model (time 0 = first picture's earliest PTS), same refusal wording (`refusal_text_for`, `in.mpg` vs `in.ts` in the ffmpeg hint). DVD LPCM added to the audio kinds (`pcm_dvd`, 16-bit BE and 20/24-bit via the PcmS24Le path).
- `Demuxer::Ts` is the one variant for both (no second variant, to avoid duplicating ~25 match arms); doc updated. Call sites `is_mpegts` became `is_mpeg_system` in: demux.rs (open, header_streams, sync_points), audio.rs (3), subtitle.rs, mcp `tools/media.rs`; `container_name` has a PS branch.
- `lib.rs` `is_audio`: `.mpg`/`.mpeg` stay audio unless the first 64 KiB shows a program stream with a picture (or raw video); this is the one place bytes are looked at. Nonexistent files keep the old extension answer.
- export.rs: one hunk, `is_mpegts` -> `is_mpeg_system` in the existing `copyable` expression. PS is never stream-copied (re-encodes; `copy_track` says uncopyable).
- `tests/mpegps.rs` (new): 11 tests.

## Peer-owned files
- `playback.rs` `parse_subtitles` and `app/src/subs.rs`: no change needed. PS carries no readable subtitles (VobSub refused), and `subtitle::of_media` routes PS through the empty PGS path anyway. No app extension list needed edits (none found besides subs.rs). Open dialog filters not inspected (app is peer's).

## Tests (host c, release, `--test mpegps`: 11/11)
Synthetic ffmpeg fixtures only. Covers: `-f mpeg` (MPEG-1 + MP2/MP3), `-f vob` (MPEG-2 + AC-3 / MP2), `-f dvd` (nav packs, 720x576 interlaced tff and bff, AC-3 / MP2 / pcm_dvd 16 and 24 bit), raw `.m2v` and `.m1v`, +10 s start, 33-bit wrap (near and mid-clip), two VOBs joined by bytes, DTS (refused, picture opens), sound-only PS (open error names missing video), H.264 in PS (`-f vob`/`-f mpeg`), renamed files sniffed (`.dat .mod .TOD .m2p noext .mpv`), tracker `.mod` and audio `.mpg` not claimed.
- Probe: codec, size, fps, interlaced tff/bff, 50 frames, duration within 1 frame; pictures byte-identical to ffmpeg's; mid-GOP seeks and ranges equal the sequential decode (deinterlaced included).
- Sound vs ffmpeg: MP2 3e-5, MP3 2e-6, AC-3 3e-5, LPCM16 0, LPCM24 within 1e-5, from 0/0.4/1.1/1.7 s; seek equals sequential.
- Sync: flash at frame 25, beep within 65 ms, steady after a seek (2 ms; 15 ms for LPCM).
- Export to mp4: length 2.4 s +/- 0.1, flash within 45 ms, beep within 90 ms of flash (mpg, vob, +10 s vob, dvd LPCM).
- Interlaced DVD picture has less comb than ffmpeg's woven frames.

## Gates
See bottom (filled after the final run).

## Known limits (documented in ps.rs docs too)
- Raw `.m2v` open: ec-mpegts reads the whole file once. Measured 0.3 s for a 361 MiB 720p file (warm cache), probe 0.6 s. Engine opens are not on a UI path I own, so no worker thread added; cold disks are IO bound.
- 20/24-bit DVD LPCM: a seek into a packet cannot find group boundaries (decodes noise). Edith therefore reads those tracks from the top for any seek (`TsAudio::seek`, PS + PcmS24Le only; HDMV LPCM in TS untouched). Exact but linear in seek position.
- 16-bit DVD LPCM: PES stamps from ffmpeg's dvd muxer jitter by up to one packet (503 samples, 11 ms); a seek trusts the stamp, so it can be that far off (sequential read is exact). Tested as a bound of 520 samples.
- Byte-joined VOBs (SCR reset): the epoch shift is SCR-based, so the timeline gets a gap (~0.3-0.45 s for ffmpeg clips with its 0.7 s mux lead). Sequential play is exact (120 of 120 frames, flashes at 25 and 85) but probe duration/frame_count are too long (5.12 s vs 4.8 s, 128 vs 120) and a seek past the join lands up to ~14 frames off. Seeks before the join are exact.
- ec-mpegts finding: when the last picture's PES carries no PTS (seen in an `-f mpeg` MPEG-1 + B-frames, 2.4 s clip), the stream duration is one frame short, so the last frame is not played (59 of 60). The sync test accepts 59..=60 for that reason. Fix belongs in ec-mpegts (derive the tail from temporal reference).
- DTS and VobSub are refused by name with the ffmpeg line. No VobSub fixture (ffmpeg cannot make one from text).
- MPEG-2 repeat_first_field (pulldown) not applied, as for TS.

## Gate results (final tree dcee64ba = rebased f6ba70d7 + clippy fix in tests/mpegps.rs)
Host c, single run on this tree: `cargo clippy -p engine -p edith_mcp -p edith --release --all-targets -- -D warnings` clean. Full `cargo test -p engine -p edith_mcp -p edith --release --no-fail-fast`: 3470 passed, 2 failed, the two known VA-API-only `h264_/hevc_mid_gop_seeks_match_a_sequential_decode`. (An earlier run on f6ba70d7 failed clippy on a stray doc comment in tests/mpegps.rs; fixed in dcee64ba. Two earlier killed runs ignored.)
