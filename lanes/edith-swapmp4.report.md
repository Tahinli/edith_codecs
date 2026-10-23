# edith lane swapmp4: MP4/MOV demux and mux moved to ec-mp4

Wave 2 (DONE.md), the mp4 row. Base: `codecs-integration` at `97673ee`. Branch `lane-swapmp4`. Not pushed.

## What changed
- Demux (`crates/engine/src/demux.rs`, `crates/engine/src/audio.rs`): `mp4::Mp4Reader` replaced by `ec_mp4::Mp4Demuxer`. New `crates/engine/src/mp4track.rs` is the audio-side adapter (tracks, config, sample reads by 1-based id, stts runs, edit media_time).
- Implicit SBR for AAC is kept on `mp4_aac_prime`: the rate comes from the ASC (`asc_fields`), and the decoder rate is the ec sample rate.
- Rotation: still parsed from `tkhd` by hand (`tkhd_matrix`), because ec-mp4 does not expose the matrix.
- `crates/engine/src/esds.rs` deleted (no users left).
- Mux (`crates/engine/src/mux.rs`): `mp4::Mp4Writer` replaced by `ec_mp4::Mp4Muxer`. Removed: the `patch_entry`/`swap_entry`/`name_subtitles`/`push_box` post-write file rewrite, `colr_nclx`, the av01/hvc1 placeholder patching. ec-mp4 writes avc1/hvc1/av01, colr nclx, tx3g and the track titles itself. AAC and tx3g samples are marked sync (no stss), as before.
- Test helpers switched to ec-mp4: `proxy.rs` (`write_audio_only_mp4`), `tests/audio_copy.rs`, `tests/audio_multi.rs`, `tests/audio_export.rs`, `tests/capability_matrix.rs`, `tests/export.rs`, `examples/probe_fps.rs`, and the unit tests in `mux.rs`.
- Manifests: `mp4` removed from `crates/engine/Cargo.toml`; `vendor/mp4` removed (`git rm`) with its exclude/patch entries and its comment block in the root `Cargo.toml`. `ec-mp4` added. `Cargo.lock` updated.
- `crates/app/src/importing.rs`: one comment naming `mp4::Mp4Reader` now names `ec_mp4::Mp4Demuxer`.
- The test-only pure helpers `fps_from_stts`, `first_frame_sample`, `composition_times` are restored under `#[cfg(test)]` in `demux.rs`. Their tests exist and must not be deleted. Production does not use them now.

## Step 1: call sites (before)
- Demux: `demux.rs` (Mp4Demuxer, mp4_subtitles, probe_bitrate, mp4_no_video, mp4_track_bytes, frame_rate, trim/first-sample helpers), `audio.rs` (probe_streams, unsupported, mp4_aac_prime, Ac3Track, AacTrack, run loops, copy path, helpers), `esds.rs`.
- Mux: `mux.rs` `Mp4Muxer` (open, add_audio_track, flush_*, write_subtitles, finish, patch helpers), and the mux/proxy/tests readers listed above.
- Symphonia `isomp4` (in `crates/engine/Cargo.toml`) is not part of this swap; see Left.

## Step 2: demux parity (old mp4 0.14 vs ec-mp4)
Harness: `crates/engine/src/demux/mp4_parity.rs`, committed in `4120e51`, removed in this lane because it needs the mp4 crate. Log: `~/.cache/tmp-edith-swapmp4/parity-before-swap.log`.

Corpus: `edith/assets`, `edith_codecs/fixtures`, `~/.cache/mp4demux-corpus`, and the real-library films in `fixtures/real-library-manifest.tsv`. 85 files, 143 tracks. Compared per track: rotation, edit media_time, presentation_trim, first_frame_sample, sync_in_display_order, coded size, frame rate, AAC rate and channels, sample count, per-sample bytes, sync flags, composition offsets, decode-time shift.

| Result | Files | Tracks |
|---|---|---|
| Match on every compared field | 84 | 142 |
| Mismatch (old reader defect) | 1 | 1 (`Music/reeln.mp4`) |

- `reeln.mp4` is HE-AAC and fragmented (empty moov sample tables, moof/mdat, sidx, mvex). The old reader returned bytes from file offset 0 for sample 1, marked every sample non-sync, and reported 22050 Hz where the stream is 44100 Hz with explicit SBR. ec-mp4 matches ffprobe (sample 1 at pos 1968 size 174, sample 2 at pos 2048 size 275, all keyframes). ec is correct here; this is a defect in the old reader, not a regression.
- The old AAC rate was also wrong for non-AAC tracks in the earlier check (AC-3 read as 0, mp3-in-mp4 as 96000). ffprobe agrees with ec on all of them, so only AAC is compared.

## Step 3: export (mux) parity
Probe: a temporary example (`mp4_probe_write`, not committed) that writes H.264 (x264, `bframes=0`, `aud=1`, 320x240 30 fps, 2 s) plus AAC-LC 44.1 kHz stereo and one `tx3g` cue. Built against the base tree and the new tree; both outputs in `~/.cache/tmp-edith-swapmp4/probe/`.

| Output | ffprobe -v error | Video decode md5 | Audio decode md5 (s16le) | Subtitle decode (srt) md5 |
|---|---|---|---|---|
| old (97673ee) with audio + subs | clean (0 lines) | a872ee54b83f203ae00ce0ab43d57bf5 | 54e5b33804c0edb3d4b3239f622b30ca | 730cd49e29748c199ed6a545345a542a |
| new (ec-mp4) with audio + subs | clean (0 lines) | a872ee54b83f203ae00ce0ab43d57bf5 | 54e5b33804c0edb3d4b3239f622b30ca | 730cd49e29748c199ed6a545345a542a |
| old video only | clean | a872ee54b83f203ae00ce0ab43d57bf5 | n/a | n/a |
| new video only | clean | a872ee54b83f203ae00ce0ab43d57bf5 | n/a | n/a |

- Brands (`isom iso2 avc1 mp41`), colr nclx bytes (`6/6/6`, limited), track languages (`und`, `por`), subtitle track title (`Signs`), and audio duration (90112 ticks at 44100) are identical.
- Subtitles: the first attempt wrote ec's default tx3g style, which ffmpeg renders as `<font face="Serif" size="18">`. Passing the old writer's 30-byte style record (`TX3G_STYLE` in `mux.rs`, the bytes the mp4 crate's default `Tx3gBox` wrote) made the decode identical to the old output.
- The first probe also showed that B-frame H.264 breaks the no-B-frame assumption of the muxer. That was in the probe input only; the old writer has the same assumption.

## Step 4: removed code and dependency tree
- Removed: `mp4` crate (engine dependency and `vendor/mp4`), `esds.rs`, the mux patch helpers, the demux helpers listed above.
- `cargo tree -p engine -e normal`:
```
engine v0.1.0 (crates/engine)
├── ec-mp4 v0.1.0 (edith_codecs/crates/ec-mp4)
│   ├── symphonia-format-isomp4 v0.6.0
...
```
`mp4 v` appears 0 times in the normal tree.

## Step 5: verify
- `cargo test -p engine --release --no-fail-fast` on server (`EDITH_TREE=~/.cache/wt/edith-swapmp4`): 877 passed, 56 ignored (pre-existing), 1 failed: `playback::import_refuses_what_does_not_match` (known failure). The load-flaky tests passed in this run.
- Local real-library and mp4 integration tests (`CARGO_TARGET_DIR=~/.cache/tgt-edith-swapmp4`, `real_library.toml` copied from the main edith checkout, `assets` linked from it): audio_seek, audio_ac3_film, audio_decode, audio_streams, opus_export, export_color, pgs_bitmaps, subtitles, mkv_encodings, audio_copy, audio_multi, audio_export, export, capability_matrix all pass, with no skips. The first local attempt failed only because the worktree had no `assets/`; that was an environment gap, not a code failure.
- `cargo clippy -p engine --all-targets` on server: exit 0. No clippy warning falls on a line this lane changed (checked against the diff hunks).

## Left
- Symphonia `isomp4` stays. It still reads ALAC, MP3 and FLAC inside mp4 as the fallback path in `audio.rs`. ec-mp4 has a demuxer for those codecs, but moving them is a separate change and is not needed for the mp4 row.
- Rotation parsing (`tkhd_matrix`) is hand-written, because ec-mp4 does not expose the tkhd matrix. Gap for ec-mp4, not an edith_codecs edit in this lane.
- tx3g style: `TX3G_STYLE` is an edith constant that matches the old writer. ec-mp4 has no way to set it without passing extradata.
- The parity harness is only in history at `4120e51`.
