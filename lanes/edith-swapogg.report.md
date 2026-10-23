# edith lane swapogg: oxideav-ogg replaced by ec-ogg

Wave 3 (DONE.md), the Ogg row. Base: `codecs-integration` at `10325aba4c`. Branch `lane-swapogg`.

## What changed
- `crates/engine/Cargo.toml`: `oxideav-ogg = "0.1.8"` replaced by `ec-ogg = { path = "../../../edith_codecs/crates/ec-ogg" }`.
  `oxideav-core` stays: `crates/engine/src/subtitle.rs` still uses `oxideav_core::Segment` (the parallel swapsubs lane owns it).
- `Cargo.lock`: `oxideav-ogg` removed, `ec-ogg` added (no other lock changes).
- `crates/engine/src/export.rs` `write_ogg` (the only Ogg mux call site in the engine):
  - `oxideav_ogg::mux::xiph_lace` -> `ec_ogg::xiph_lace`.
  - `oxideav_ogg::mux::open_concrete` + `oxideav_core::Muxer` (`write_header`/`write_packet`/`write_trailer`) -> `ec_ogg::OggMuxer::new(File)`, `Muxer::add_stream`, `Muxer::write_packet`, `Muxer::finish`.
  - Stream is built with `ec_core` types (`CodecParameters::new(CodecId::Vorbis)`, `MediaParameters::Audio`, `StreamInfo::new`).
  - The encoder granule now travels as `ec_ogg::granule_side_data(packet.granule)`. ec-ogg takes a packet's granule from that side data and only falls back to `pts + duration`. Using `pts + duration` would put every page one block late.
  - Comments that named `oxideav-ogg` now name `ec-ogg`.
- `crates/engine/tests/audio_export.rs`: one doc comment (`oxideav-ogg` -> `ec-ogg`). No test logic changed.
- `rustfmt` was run on `export.rs`; the only hunk outside the Ogg code (a pre-existing line in `write_flac`) was reverted to its original form.

## Audit: every oxideav-ogg use in the engine
- Mux: `export.rs` `write_ogg` only (3 calls). Replaced as above.
- Demux: none. Engine Ogg/Opus input goes through `symphonia` (`audio.rs` probe/codecs). Nothing to map, and nothing demux-side changed.
- Opus pre-skip (DONE.md names `mux.rs:1715-1731`): that code is the Matroska `A_OPUS` CodecPrivate writer (`opus_track_entry`). It builds its `OpusHead` locally and never touched oxideav-ogg. Edith has no Ogg-Opus writer, so there is no Ogg Opus parity to measure. The Matroska path is unchanged.
- ec-ogg API gaps: none found. The API covers the write path used here.

## Parity (Vorbis-in-Ogg export, old vs new)
Inputs: the two existing tests' exports, a 3 s stereo mix (`test_av.mp4`/`test_tone.mp3` timeline, 44.1 kHz) and a 1 s mono mix (synthetic 440 Hz). Old = oxideav-ogg, new = ec-ogg. Produced on the server by the same test binary with a temporary copy hook (removed before commit).

| Output | Bytes old / new | Byte-identical | Packets (ffprobe pts, duration, size) | Decoded PCM (ffmpeg, s16le) | Last granule / duration |
|---|---|---|---|---|---|
| stereo 3 s | 34706 / 34706 | no (1673 bytes differ) | identical | identical, md5 `438dbe4f...`, 529200 bytes = 132300 frames | 132300 / 3.000000 both |
| mono 1 s  | 6072 / 6072   | no | last pts 45056 (old) vs 44032 (new) | **differs**: old 180224 bytes (45056 frames), new 176400 bytes (44100 frames) | 44100 / 1.000000 both |

- `ffprobe -v error` is clean on all four files (0 stderr lines).
- Stereo page layout differs only in how the header packets are grouped. Old: comment page, setup page, then audio. New: comment and setup on one page, then the first audio packet alone on a granule-0 page. Audio page granules are identical (26624, 35904, 50176, 132300).
- Mono: old has 4 pages, with the first audio data landing on the EOS page; new splits the first data page at its first granule (ec-ogg's `first_page_pending` rule). ffmpeg decodes the old mono file 956 frames longer than the timeline; the new file decodes to exactly 1.000 s. The engine's own decoder reads both as exactly 1 s, and `a_mono_timeline_exports_as_dual_mono_ogg` asserts that (passes on both).
- So the stereo output is "identical decoded audio, identical granules, pre-skip N/A" per the task's rule, but not byte-identical. The mono output is intentionally different: the new file is the one that matches the timeline length in ffmpeg. This is a behaviour change, and it is the reason to keep the mono test in the verify list.
- Demux: nothing changed in demux, so there is no demux comparison to make.

## Verify (remote-run, `EDITH_TREE=~/.cache/wt/edith-swapogg`, host a or b per run)
1. Ogg-related test files: `cargo test -p engine --release --test audio_export --test audio_files --test export --test opus_export --test backends --test capability_matrix --test audio_decode --test audio_seek --test audio_ac3_film`
   - all pass: audio_ac3_film 2/2, audio_decode 11/11, audio_export 15/15, audio_files 20/20, audio_seek 8/8, backends 8/8, capability_matrix 4/4, export 28 passed + 6 ignored (pre-existing ignores), opus_export 5/5.
2. Full suite: `cargo test -p engine --release --no-fail-fast`. Exit 101 with exactly the three failures the brief lists as known:
   - `playback::import_refuses_what_does_not_match` (known failure)
   - `tonemap::tests::perf_4k_and_1080p` (known load-flaky)
   - `bitrate::no_decoder_is_opened` (known load-flaky)
   - The other known-flaky tests (`a_caption_window_no_project_can_hold_is_refused_by_name`, `subtitles::a_project_naming_many_tracks...`) passed.
   - Every other test binary: `test result: ok`.
3. Re-run after the formatting-only edit: `cargo test -p engine --release --test audio_export` 15/15 pass.
4. `cargo clippy -p engine --all-targets`: exit 0, 165 warning/error lines across the engine (pre-existing, not gated by `-D warnings`). None are in lines touched by this lane. The export.rs diagnostics are at 1099, 4081, 4374, 4442/4443, 4519, 4535, 4989, 5157, all outside the hunks at 66/73/214/3884/3960-4010.
5. `cargo tree -p engine -i oxideav-ogg`: "package ID specification `oxideav-ogg` did not match any packages", i.e. no path to oxideav-ogg.

## Left
- `oxideav-core` cannot be removed from `crates/engine/Cargo.toml` until swapsubs lands: `subtitle.rs` still uses `oxideav_core::Segment` (line 46 and the parser shims at 90-114, 522-611).
- The DONE.md wording points the Opus pre-skip gap at `mux.rs:1715-1731`. That is the Matroska writer, not Ogg. DONE.md should say so, or the item can be closed as N/A for Ogg.
- Mono: the new ec-ogg output differs from the old one (see parity). Recorded as intentional; the old file was the one ffmpeg got wrong.
