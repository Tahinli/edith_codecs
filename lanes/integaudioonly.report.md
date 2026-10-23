# Lane integaudioonly: report

Goal: audio-only mp4 and m4a export on the ec-mp4 writer, which refused it with "audio-only mp4 and m4a export is not on the ec-mp4 writer yet".

Branches (nothing pushed, main checkout untouched):
- edith_codecs: `lane/integaudioonly`, worktree `~/.cache/wt/integaudioonly`, base main `011e0b4f`. Commit `fba06a66`.
- edith: `lane-integaudioonly`, worktree `~/.cache/wt/edith-integaudioonly`, base codecs-integration `24ac835`. Commit `b17841a`.

## Changes

1. `crates/ec-mp4/src/mux.rs`: `Mp4Muxer::with_brands(w, major, compatible)` sets the `ftyp` brands. `new` keeps the old default (`isom`, the video codecs' brands, `mp41`). Unit test `an_audio_only_m4a_states_its_brand_and_holds_one_aac_track`: one AAC track, `M4A ` major, no `vide` anywhere, reads back the same ten packets.
2. `crates/engine/src/mux.rs`: `Mp4Muxer::create_audio_only` now builds an ec-mp4 writer with `M4A ` (compat `M4A `, `mp42`, `isom`) for `.m4a`, or `isom` (compat `isom`, `iso2`, `mp41`) for a sound-only `.mp4`, the same brands main used. New fields `has_video` and `audio_stream`: a sound-only file has no picture, so `finish` skips the picture checks, `drain` writes audio as it arrives, and the AAC track is stream 0. The export wiring already existed (`export.rs` `run_aac`), so no export change was needed.

No video track, no separate moov/mvhd timescale change: ec-mp4 already writes an audio-only moov with its 1 kHz movie clock (durations round up to a millisecond).

## Verify (host c, `--with-edith`)

- `cargo test -p ec-mp4`: all green (lib 26 passed, including the new test; integration suites 2, 1, 16 (1 ignored), 10, 3).
- `cargo clippy -p ec-mp4 --all-targets -- -D warnings`: 0 warning/error lines.
- `cargo test -p engine --test audio_files --test audio_only_mp4_reopen` (debug): audio_files 22 passed; audio_only_mp4_reopen 2 passed. The three target tests pass:
  - `audio_files::an_audio_only_mp4_is_the_same_aac_under_an_mp4_brand` (ok)
  - `audio_files::an_m4a_export_is_one_aac_stream_under_the_m4a_brand` (ok)
  - `audio_files::an_audio_only_project_saves_reloads_and_exports_its_sound` (ok)
- The audio_files helper checks each output with ffprobe: `stream=codec_name,codec_type` is exactly `aac,audio`, `format=duration` is 3.0 s within 0.05 s for a 3 s tone, and the major brand is `isom` (mp4) or `M4A ` (m4a).
- `cargo test -p engine --release --no-fail-fast`: **passed 1181, failed 16, ignored 57** (87 result blocks). Before this lane: 1180 / 17 / 57.

### Failures in the final run

Fixed by this lane: the three audio-only tests above.

Known, not this lane (listed in `~/.cache/wt/edithinteg.report.md`):
- AV1 4:2:2/4:4:4: `colorspace::tests::each_codec_states_its_chroma_format`, `av1_software_export::av1_422_and_444_compose_and_12_bit_is_refused`, `export::chroma_420_tests::every_av1_fixture_arrives_as_420`
- Subtitles: `subtitle::tests::a_bad_timing_line_is_skipped_and_named_with_its_line`, `subtitle::tests::vtt_escapes_what_it_must_and_roundtrips`
- `audio_streams::probe_streams_lists_every_audio_track_in_file_order` (bitrate not ported)
- `decode::tests::a_span_with_no_seat_at_all_reports_the_gap`
- mp4 AAC probe: `r14_probe_undo_streams::unsupported_flags_only_what_cannot_decode`, `export::planned_sound_tests::an_untouched_aac_mp4_is_still_predicted_as_a_copy`
- `export::a_copied_sound_runs_as_long_as_its_picture`
- `container_sniff::a_range_extensions_hevc_is_refused_by_name_before_the_plugin`
- `dependency_guard::no_forbidden_crate_is_in_the_dependency_graph`
- Host c has no VA-API plugin: `hw_midgop_seek::hevc_mid_gop_seeks_match_a_sequential_decode`, `::h264_mid_gop_seeks_match_a_sequential_decode`

Not diagnosed, both pass when run alone:
- `playback::paste_at_eos_revives_the_session` failed in the full run; alone it passes (4.9 s). Looks like load.
- `tonemap::tests::perf_4k_and_1080p` is a timing budget (33 ms per 3840x1600 frame against 25 ms, or 54 ms under full load). It fails on the shared host. It does not touch the mux code.

## Left

- The ec-mp4 movie clock stays at 1 kHz for sound-only files. Durations round up to a millisecond, which is well inside the 0.05 s the tests allow.
- `write_subtitles` on a sound-only file is not reachable from export (`run_aac` writes no subtitles).
