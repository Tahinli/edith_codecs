# lane opusfile — standalone Ogg Opus export

Commit 63b5bec on lane-opusfile (base c79ab6a). Not pushed.

- Format::Opus (.opus) writes Opus in Ogg per RFC 7845, mixed at 48 kHz. The new module is crates/engine/src/opus_write.rs: ec-opus encodes each stream CELT-only, and ec-ogg's OggMuxer pages it with OpusHead/OpusTags supplied as header packets.
- OpusHead: channels, pre-skip 1080 (120 look-ahead plus one warm-up frame), input rate, family 0 or family 1 with streams/coupled/table.
- OpusTags: vendor ec-opus, ENCODER=edith (ec-opus), LANGUAGE, and CHAPTERxxx/NAME. There is no title setting in ExportSettings.
- Granules are at 48 kHz with pre-skip; the last granule trims the padding, so the decode is exact to the sample.
- Rate: audio_kbps clamped to 6–510, 128 by default. Loudness, range, cancel per packet, and progress all follow the run_audio path.
- New preset podcast_opus: 96k, -16 LUFS / -1 dBTP.
- ffprobe format duration is the last granule over 48 kHz with pre-skip included. ffmpeg's own libopus files behave the same way: a 3 s file probes at 3.0065 s. The tests check ffprobe minus pre-skip to ±1 ms, plus the decoded sample count.
- Results: stereo 128k SNR is 25.0 / 25.2 dB (libopus 128k on the same signal: 29.3 / 29.7). 5.1 at 256k: 27.0, 25.3, 33.6, LFE 11.7, 28.7, 27.5 dB, with no cross-channel leakage. Ranged SNR is 24.0 dB. podcast_opus measured -15.9 LUFS.
- Gates: clippy -D warnings is clean, and cargo check -p edith passes. On server c, 2627 passed and 3 failed. Two failures are the known h264/hevc mid-GOP seek tests. The third was an mcp help test my wording broke; it is fixed, and edith_mcp now passes 1100/1100 locally.
