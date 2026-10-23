# lane/mpegps report

Branch lane/mpegps (worktree /home/user/.cache/wt/mpegps), commits prefixed `lane/mpegps: `, not pushed.

## Result
`PsDemuxer` in `crates/ec-mpegts` (`src/ps.rs`, `src/psvideo.rs`): MPEG-1 system streams, MPEG-2 program streams (.mpg .mpeg .vob .m2p) and raw MPEG-1/2 video (.m2v .mpv). Gates: clippy workspace `-D warnings` clean; `cargo test -p ec-mpegts --release` with EC_REQUIRE_FIXTURES=1 passes on server host a (TS tests unchanged); fuzz `demux_ps` 5 min on host a, 15k runs, no crash/hang/OOM; DONE.md has a PS subsection.

## API (for edith)
- `PsDemuxer::new(reader)` (Read+Seek), implements `ec_core` registry `Demuxer` (streams, next_packet, seek SyncBefore/SyncAfter/Exact, stats, duration). Time base 1/90000.
- Extras: `kind() -> PsKind{Mpeg1System, Mpeg2Program, VideoElementary}`, `tracks()` (with refusal text), `stream_id`, `stream_key`, `time_base`, `duration_secs`, `stats` (discontinuities counts SCR resets), `epoch_count`, `last_packet_offset`, `set_video_framing(VideoFraming)`.
- Sniffing: `is_mpegps(head) -> bool`, `is_mpeg_video_es(head) -> bool` (export from `ec_mpegts`).
- Track key/pid = `(stream_id<<8)|substream` (e.g. 0xE000 video, 0xC000 audio, 0xBD80 AC-3, 0xBDA0 LPCM, 0xBD20 VobSub).
- Codecs: `Mpeg2Video` (MPEG-1 too), H.264/HEVC Annex-B (PSM or content-sniffed), Mp1/Mp2/Mp3, AAC ADTS, AC-3, DVD LPCM (16-bit PcmS16Be; 20/24-bit PcmS24Le).
- Raw ES is handled inside PsDemuxer (no separate EsDemuxer; less code). Open indexes the whole file once; seek is an index lookup; pts from frame rate and temporal reference.

## Behaviour notes
- SCR backwards by more than 0.5 s starts a new epoch; timestamps stay monotonic (concatenated VOBs, cell joins). Files over 96 MiB are checked by sampling 256 SCRs (backward steps only).
- Pictures with no PES timestamp get derived dts/pts (ffmpeg leaves pts unknown).
- After a seek, pictures sharing the landing PES but preceding the keyframe are dropped; seek-then-read equals the sequential read from the landing offset.
- A file starting mid-GOP opens and delivers non-key pictures first; a seek before the first keyframe lands at the start.

## Refusals (named in tracks())
DTS/DTS-HD (`dts`), VobSub ("DVD subtitles (VobSub)"; no decoder exists), other private_stream_1 substreams, LATM, unknown PSM stream types. private_stream_2 and padding are skipped.

## Known limits
20/24-bit DVD LPCM seek may land mid 2-frame group (no seek check); PAFF field pictures are one packet per picture header; epoch shift is SCR-based so approximate across a join; corrupt SCR outliers can shift the timeline; raw ES open reads the whole file.

## Fixed late
Panic in `find_pack` (pack header shorter than its length); MPEG-2 slice codes mis-sniffed as H.264 on mid-GOP starts.

## Tests
`tests/ps.rs` (24, ffmpeg-generated; skip without ffmpeg), `tests/committed_ps.rs` (4 fixtures, ~206 KB in tests/data with gen_ps.sh, expected_ps.txt), fuzz target `crates/ec-mpegts/fuzz/fuzz_targets/demux_ps.rs`, debug tool `examples/psdump.rs`.
