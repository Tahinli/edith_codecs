# Lane edith-mpegts report

Branch `lane-mpegts` (worktree `/home/user/.cache/wt/edith-mpegts`), rebased onto `3feda907` (format v46) with no conflicts. Commits: `997805d2` (MPEG-TS), `c8005c44` (clippy clean-ups), `f56a8796` (MPEG-2 video + MPEG audio layers I/II, tests).
Commits use gitmoji messages and carry no trailers.

## What changed (extension: MPEG-2 video, MP2/MP1)

- **MPEG-2 video** (stream types 0x01/0x02, `CodecId::Mpeg2Video`) is now decoded, always in software, by `ec-mpeg2`:
  - `demux.rs`: new `Codec::Mpeg2` (name "MPEG-2"). Reachable only from a transport stream; the Matroska `V_MPEG2` row in `UNSUPPORTED` stays (an mkv with MPEG-2 is still refused as before, no work was asked there).
  - `demux/ts.rs`: the sequence header + extension is the config, re-injected ahead of any keyframe that lacks one (a seek landing on a bare I picture still decodes). Profile is read as ffprobe spells it (Simple, Main, High, 4:2:2). `Demuxer::h264_interlaced()` is true for an MPEG-2 sequence with `progressive_sequence` 0 (name kept, doc says so).
  - `decode/mpeg2.rs` (new): `Stream` + `run_mpeg2`, the same shape as the HEVC/VP8 seats. Registered in `Pulled` (`decode.rs`), so playback, scrub, thumbnails and export all read the one decode. `hw_decodes` refuses MPEG-2 by name ("plays on the ec-mpeg2 software seat"), engine-hw has a no-op arm (`Codec::Mpeg2 => return None`).
  - Interlace: an interlaced sequence goes through the existing `Deinterlacer`, picture by picture (new `Deinterlacer::flagged_only()`: a picture coded progressive in an interlaced sequence, i.e. a film cadence, passes untouched; only pictures that state a field order are filtered). `deinterlace::field_order` reads the first interlaced picture's order from ec-mpeg2 (probe reports `tff`/`bff`). The clip's `Deinterlace` setting is honoured the way H.264 honours it: stored only, behaviour is the `Frame` one; when the other lane makes the setting act, the Mpeg2 seat uses the same `Deinterlacer` so it follows.
  - 4:2:2 (`I422`) is averaged down a pair of chroma rows to 4:2:0 by `Deep::load`, exactly as H.264 4:2:2.
  - Seeks: land on the keyframe at or before the target (one earlier for an interlaced sequence, for the deinterlacer's predecessor). ec-mpeg2 drops the open-GOP B pictures that lack a forward reference, as ffmpeg does; they sit before the landing I picture in display order so the display index counts from the I picture. A seek equals the sequential decode (tested, deinterlaced too).
  - `colorspace.rs`: MPEG-2 colour tags from the sequence display extension, chroma from the sequence extension.
- **MPEG audio Layers I and II** (`CodecId::Mp1`/`Mp2`): routed to `ec-mp3` through `ec_mp3::layer_codec_parameters`, the same `TsDecoder::Mp3` that serves Layer III. Probe names them `mp1`/`mp2`/`mp3`, decodable.
- **Refusals left**: DTS, TrueHD (and DTS-HD), LATM. The old MPEG-2 and MP2 refusal tests were removed.
- Cargo.toml: second added line (see below).

## What changed (first round)

- `crates/engine/src/demux/ts.rs` (new, a child of `demux.rs`):
  - `is_mpegts(path)` recognises a transport stream by extension (`ts mts m2ts m2t tp trp`) or by content. A file with an EBML or `ftyp` magic is never taken for one.
  - `TsDemuxer` is the picture reader. It returns Annex-B access units with SPS/PPS re-injected on keyframes that lack them. Frame count and rate come from `ec-mpegts` timing. Seeks use `SyncBefore` and return the display index of the keyframe they land on.
  - `TsAudio` and `TsDecoder` read one sound track through their own demuxer, so a seek on the sound alone lands where the sound is. They decode AAC-ADTS (`ec-aac`), AC-3 and E-AC-3 (`ec-ac3`, folded to a stereo pair by the decoder), MP1/MP2/MP3 (`ec-mp3`) and HDMV LPCM (`PcmS16Be` and `PcmS24Le`). A seek reopens or Exact-seeks backwards with growing slack until the first audio packet is at or before the target, then runs a 0.35 s pre-roll through a fresh decoder.
  - `subtitles()` returns PGS tracks as the same `MkvSubtitle` display sets that `subtitle::pgs_cues` already reads.
  - `header_streams()` feeds the probe.
  - `refusal_text()` appends the ffmpeg conversion to a refusal.
- `demux.rs`: new variant `Demuxer::Ts` with a match arm in every `Demuxer` method. Also `probe_bitrate`, `stated_secs`, `sync_points` (returns empty), `header_streams`, `container_name` (`mpegts`, or `mts`/`m2ts`) and the pub re-export `demux::is_mpegts`.
- `audio.rs`:
  - New `Track::Ts(Box<TsAudio>)`, with `run_ts` and the `Track::open`, `segment`, `check_decoder`, `copy_track`, `probe_streams` and `unsupported` hooks. It is never stream-copied: `copy_track` reports "uncopyable" and the export re-encodes.
  - Refused sound tracks (DTS, TrueHD, LATM) are listed by `probe_streams` as `decodable: false` under their ffprobe name. Opening one yields a silent source. `unsupported()` names it with the ffmpeg line.
  - `run_ts` places each packet by its timestamp. Sound that starts after the picture leaves leading silence. Sound that starts before frame 0 is trimmed.
- `subtitle.rs`: `of_media` now dispatches to the new `of_mpegts`.
- `mcp/src/tools/media.rs`: `"hdmv_pgs_subtitle"` is named `pgs`, and a transport stream skips the mp4 subtitle walk.
- `crates/app/src/subs.rs`: one line, the extension list in `carries_subtitles` now includes `ts`, `m2ts`, `mts` and `m2t`. This is the exception you approved.
- `crates/engine/tests/mpegts.rs` (new): 9 tests.

**Time model.** Source time 0 is the first picture's earliest PTS, because the engine's frame 0 sits at t=0. A file with no picture counts from its first sound.

This differs from "earliest stream start". Sound that starts before the picture is trimmed to the picture, which keeps the A/V offset.

## Cargo.toml

Two lines added to `crates/engine/Cargo.toml`, each with a comment above it:

`ec-mpegts = { path = "../../../edith_codecs/crates/ec-mpegts" }`

`ec-mpeg2 = { path = "../../../edith_codecs/crates/ec-mpeg2" }`

## export.rs hunks

One hunk. The `copyable` expression near line 2945 gains `|| crate::demux::is_mpegts(&source.path)`. This only affects the card label, so it names a re-encode instead of a copy. `CopyPlan` already refuses non-Matroska sources, so a TS export always re-encodes.

## GUI and peer-owned changes needed

- **`crates/engine/src/playback.rs`, `parse_subtitles` (about line 1798, peer-owned, NOT edited).** The exact diff for its owner:

  ```diff
           let tracks = match crate::demux::is_matroska(path)
  +            || crate::demux::is_mpegts(path)
               || path.extension().is_some_and(|e| {
  ```
  (`is_mpegts` is a pub re-export from `engine::demux`; it does a cheap extension check, then a content sniff for unknown extensions.) Background: PGS from an `.m2ts` does NOT flow through the `subs.rs` gate alone. `parse_subtitles` only calls `subtitle::of_media` for Matroska and `mp4|m4v|mov`. Any other extension is read as a standalone subtitle file and refused. The needed change:
  - add `|| crate::demux::is_mpegts(path)` to the condition that selects `subtitle::of_media`, or
  - add `"ts" | "m2ts" | "mts" | "m2t"` to its extension list.
  - The engine side (`subtitle::of_media` to `of_mpegts`) is ready.
- File dialog: `pick_file` in `crates/app/src/files.rs` was not inspected for extension filters. No extension list was found in `crates/app/src` other than `subs.rs`.

## Tests (server host c, release)

`cargo test -p engine --release --test mpegts` passes 12 of 12 on the final tree (server host c, ffmpeg-skip convention kept). All sources are synthetic: testsrc2, a flash on black, a chirp and a beep.

- **Picture vs ffmpeg:** `.ts`, `.m2ts`, `.mts`, a `.ts` with a start offset, and a video-only `.ts`. Probe gives codec, 352x288, 25 fps, 50 frames, and duration within 1 frame. The ec-h264 planes are byte-identical to ffmpeg's.
- **Seeks and remux:** H.264 and HEVC (including a +10 s start). `open_at` at 14 targets and `open_range` equal the sequential decode. Sequential frames are bit-identical to the mp4 remux of the same stream.
- **Sound vs ffmpeg:** AAC, AC-3, MP3, LPCM, and a two-audio `.mts` (AC-3 plus pcm_bluray). Compared from 0, 0.4, 1.1 and 1.7 s with the picture-relative offset applied. Largest difference: AAC 1.6e-5, AC-3 2.9e-5, MP3 2e-6, LPCM 0 (the tolerance was 1.5e-3, or 1e-6 for LPCM). A seek equals the sequential decode within 1e-4.
- **Sync:** flash at frame 25 (1.0 s) and beep at 1.0 s, with the beep within 65 ms. Covers start 0, start 10 s, 2 s before the 33-bit wrap, and a wrap in mid-clip. After a seek the beep stays within 2 ms and the picture seek lands on frame 25.
- **Fake AVCHD:** 1920x1080 tff, AC-3 plus pcm_bluray, 192-byte packets, `.mts`. `interlaced == "tff"`, 20 frames, planes equal to ffmpeg's, 20 deinterlaced session frames, both sound tracks decode, and the mp4 export has the right duration and frame count and has sound.
- **Export:** `.mts` (AC-3, +10 s) and `.ts` (AAC) to mp4. Length is 2.4 s ± 0.1, flash within 45 ms of 1.0 s, and beep within 90 ms of the flash.
- **MPEG-2 video** (`mpeg2_video_opens_probes_and_decodes_as_ffmpeg_does`): progressive `.ts` (MP2 sound), `.m2ts` with +10 s start (AC-3), interlaced tff, interlaced bff, 4:2:2 profile. Probe: codec Mpeg2, 352x288, 25 fps, 50 frames, duration within a frame, `interlaced` tff/bff/None, profile Main/4:2:2, `mpeg2video` in the stream list. ec-mpeg2's planes through the engine's demuxer are byte-identical to ffmpeg's (4:2:0 and 4:2:2). The session yields frames 0..50.
- **MPEG-2 seeks** (`mpeg2_seeks_equal_a_sequential_decode`): same 5 shapes, `open_at` at 14 targets and `open_range` equal the sequential session frames bit for bit (deinterlaced shapes included).
- **Deinterlace** (`interlaced_mpeg2_is_deinterlaced_and_progressive_mpeg2_is_not`): the session's frames of an interlaced file have less row-to-row comb energy than ffmpeg's woven frames.
- **MP2 sound** is in the sound test (`mp2.ts` with H.264 video, `dvb.ts` MPEG-2 + MP2 at +10 s): worst difference vs ffmpeg 3.2e-5 at 0, 0.4, 1.1, 1.7 s, and a seek equals the sequential decode. Sync test gained three DVB cases (MPEG-2 + MP2 at start 0, 10 s and a 33-bit wrap in mid-clip): flash on frame 25, beep within 65 ms, steady after a seek. The export test gained `dvb.ts` and `dvb-off.ts` (MPEG-2 + MP2, start 0 and 10 s): length 2.4 s +/- 0.1, flash within 45 ms, beep within 90 ms of the flash. A renamed MPEG-2 `.dat` opens by content.
- **Refusals** (`dts_and_truehd_tracks_are_refused_by_name_and_the_picture_still_opens`): DTS and TrueHD tracks are listed `decodable: false`, `unsupported()` names them with the ffmpeg line, the picture still opens (skips if ffmpeg cannot mux the fixture). LATM has no fixture (ffmpeg cannot put it in a transport stream).

## Gates

- `cargo clippy -p engine -p edith_mcp -p edith --release --all-targets -- -D warnings` is clean on host c.
- Full test: `cargo test -p engine -p edith_mcp -p edith --release --no-fail-fast` on host c after the final commit `f56a8796`: 3411 passed, 2 failed, and the 2 are the known VA-API-only `h264_mid_gop_seeks_match_a_sequential_decode` and `hevc_mid_gop_seeks_match_a_sequential_decode`. `tests/mpegts.rs`: 12 of 12. Clippy (`-p engine -p engine-hw -p edith_mcp -p edith --release --all-targets -- -D warnings`) clean on the final tree.
- **An earlier full run (run 2) had 103 failures**, almost all in `saveload`, `sequences`, `speed*`, `stabilise`, `subtitles`, `transitions` and friends, while another lane's suite ran on host c at the same time. Every one panicked at the fixture copy (`tests/saveload.rs:29`, `std::fs::copy(asset(name), ...).expect("copy the fixture")`): the lane's `edith/assets` was missing or half-synced at that moment. It is not shared `$HOME` state: the tests' scratch directories come from `std::env::temp_dir()` (`engine::scratch::fresh`, per-lane `TMPDIR`, pid-named), and the only fixed paths in the engine are `src/project/sequence.rs:1164` (`/tmp/edith-sequence-test*.mp4`, a unit test that only names paths) and the cache dirs, which honour `XDG_CACHE_HOME` (`proxy.rs`, `stabilise.rs`). The assets come from `remote-run.sh`'s rsync of the main checkout's `assets/` (`EDITH_ASSETS`), so a concurrent change or sync of that tree, or a lane-level race in the tree sync, explains it. Re-running the same binaries alone passed, and the third full run (same tree, host c free) was clean. No isolation fix is needed in project.rs/edith.rs; a possible hardening is for `remote-run.sh` to sync assets into a per-lane directory atomically (rsync to a temp name, then rename).
- No commit has trailers (checked `git log 3feda907..HEAD`).

## Known limits

- MPEG-2 `repeat_first_field` (3:2 pulldown) is not applied: one coded picture is one timeline frame, so a pulled-down 29.97 file plays at its coded rate with no repeated fields.
- A Matroska file holding MPEG-2 (`V_MPEG2`) is still refused: the MPEG-2 seat is reachable from transport streams only.
- No MPEG *program stream* demuxer exists in edith: `.mpg`, `.mpeg`, `.vob` and `.m2v` (raw elementary stream) are not opened (`lib.rs` lists the extensions as non-audio and the open refuses them); this lane did not build one. `ec-mpeg2`'s streaming API (`send_bytes`) would serve a raw `.m2v` reader, and `.mpg`/`.vob` need a PS demuxer in `ec-mpegts` or a sibling crate.
- MPEG-1 video inside a TS (type 0x01) takes the same path as MPEG-2.

- BD LPCM with more than 2 channels is not reordered to film order.
- A keyframe that is a non-IDR recovery point with leading B-pictures gets a seek display index from its PTS only.
- `sync_points` is empty for TS, so reverse play plans loosely.
- Only the first sound track feeds the bitrate card.
- Time spent on a PGS import is one full demux of the file.
