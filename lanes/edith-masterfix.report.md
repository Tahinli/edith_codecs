# lane masterfix report

Branch lane-masterfix (from b6162ac3), 2 commits, not pushed, no trailers.

## Changes
- export.rs: new `ExportSettings::audio_bit_depth: Option<u8>` (16 or 24; None/other = 24, ProRes only). `prores_audio_bits()` helper. `mix_pcm(.., bits)` scales the f32 mix straight to full scale of the width (32767 / 8388607) so there is no 16-bit step. `run_audio` passes 16 (WAV/FLAC/MP3/OGG/Opus byte-identical). `mix_pcm16` threads `bits`. `write_sound(.., bits)` call. HEVC mp4 path: `mp4.set_hdr(&settings.hdr)` when transfer != Sdr. HevcParams literals get `light`.
- mux.rs: `Mp4Muxer.light` + `set_hdr` + `set_content_light` in finish (default writes nothing); `HevcParams.light`; `MkvMuxer::open(.., light)`; `write_light` writes Colour children MaxCLL 0x55BC, MaxFALL 0x55BD, MasteringMetadata 0x55D0 (0x55D1-0x55D8 chromaticities, 0x55D9 LuminanceMax, 0x55DA LuminanceMin); `hdr_light()` helper. SDR passes None.
- prores_out.rs: `write_sound(.., bits)`: PcmS24Be (in24 + chan by ec-mp4) or PcmS16Le.
- crates/app/src/export_ui.rs (compile-forced, one line): `audio_bit_depth: None,` in the full ExportSettings literal. For the app: expose the field (16/24) if wanted; default 24.
- tests: prores_export.rs (codec pcm_s24be, bits 24, channels/channel_layout mono and stereo, samples within 1 LSB of a 24-bit reference, all 256 low-byte values used, audio_bit_depth=16 gives pcm_s16le); hevc10_export.rs (ffprobe -show_streams and -show_frames show Mastering display + Content light level with exact values, mp4 and mkv; SDR HEVC mp4 has none). Existing probe() in hevc10 now takes the first line and trims the trailing comma, since container side data adds a csv row.

## Findings
- 5.1 `chan` is not reachable from the export: the mixer folds to stereo, so only mono/stereo are tested.
- ffprobe prints Matroska stream mastering values as reduced rationals, mp4 stream and SEI frame values unreduced.

## Gates (host c, final commit)
- clippy -p engine -p edith_mcp -p edith --release --all-targets -D warnings: clean.
- Full test: 3626 passed, 3 failed: the 2 known VA-API (h264_/hevc_mid_gop_seeks_match_a_sequential_decode) plus a_trim_widened_past_the_analysis_is_stabilised_end_to_end, which passes alone in 6 s (timing flake under load).
- export_default_bytes green.
