# edith lane aacall: every AAC stream through ec-aac

Base: edith main `b7d8ed0`. Branch `lane-aacall`.

## Goal
All AAC decoding in edith goes through ec-aac (edith_codecs DONE.md, ec-aac). symphonia refused
SBR ("aac: aac too complex"), so stereo HE-AAC files did not open.

## What changed
- `crates/engine/src/audio.rs`
  - `aac_decoder` always builds an `ec_aac::AacDecoder`: from the track's AudioSpecificConfig when
    it has one (mp4 esds, Matroska CodecPrivate), otherwise from the first ADTS frame. symphonia's
    AAC decoder is no longer used anywhere.
  - Track rate is the decoder's output rate (`aac_output_rate`): the SBR extension rate for HE-AAC,
    the core rate otherwise. Applies to the mp4 `AacTrack` and the Matroska/ADTS `SymTrack`.
  - mp4 `AacTrack` opens AAC-LC and HE-AAC v1 (SBR). HE-AAC v2 (PS) is refused by name, since ec-aac
    returns core-only audio for PS. HE-AAC tracks cannot be packet-copied (the copy rebuilds an LC
    config), so export re-encodes them.
  - `audio_streams` lists HE-AAC as decodable.
- `crates/engine/src/esds.rs` (new): reads the raw AudioSpecificConfig from the file's `esds`
  box. mp4 0.14 keeps only the core's frequency, which is not enough for SBR. Handles QuickTime
  v1/v2 sample entries and `wave` wrappers.
- `crates/engine/src/adts.rs` (new): standalone ADTS reader as a symphonia `FormatReader`.
  symphonia ships its ADTS reader inside the `aac` feature, so removing that feature required it.
  Frames are counted at open, and the whole frame (header included) goes to ec-aac.
- `crates/engine/Cargo.toml`: removed `symphonia-codec-aac` and symphonia's `aac` feature. `isomp4`
  and `mkv` stay. `Cargo.lock` loses only the `symphonia-codec-aac` entries.
- `crates/engine/tests/audio_aac.rs` (new): four tests.
- `crates/engine/tests/data/aac_he_v1_stereo.m4a` (new): 2.04 s stereo HE-AAC v1, 8.8 KB.

## Fixture note (decision for the owner)
ffmpeg here cannot encode HE-AAC (libfdk_aac fails with "Unable to set the AOT 5"), so the HE
fixture is a 2 s stream copy of a local file, `Music/reeln.mp4`. It is a music clip, so
review it before merging, or swap it for a fixture you are happy to commit.

## Tests
- `cargo test -p engine --release --test audio_aac`: 4/4 pass.
  - `stereo_he_aac_opens_at_its_sbr_rate_and_decodes`: 44100 Hz, 2 ch, about 2 s, nonzero RMS on both channels.
  - `stereo_he_aac_in_matroska_decodes_at_the_same_rate`: same file remuxed to mkv with ffmpeg.
  - `stereo_aac_lc_matches_ffmpeg_per_channel`: ffmpeg-encoded LC, correlation >= 0.999 per channel
    against ffmpeg's decode. The old symphonia output was not captured before the change, so the
    comparison is against ffmpeg, not against symphonia.
  - `symphonia_codec_aac_is_not_an_engine_dependency`: Cargo.toml text guard.
- `cargo tree -e normal --workspace -i symphonia-codec-aac`: the package is not in the tree.
- Full `cargo test -p engine --release --no-fail-fast`, run locally with `assets/` linked (the server
  sync excludes `assets/`, so the server run gave 43 failures from missing media, which is not a
  code result). Local result: 825 passed, 11 failed, all but one also failing on clean main:
  - `a_quicktime_mov_keeps_its_aac_behind_a_wave_wrapper` (audio_decode): failed on my first
    version of esds.rs, fixed in `esds.rs`, passes after the fix.
  - `import_refuses_what_does_not_match` (playback): fails identically on clean main, same line
    (`playback.rs:1227`). Pre-existing, not caused by this lane.
  - 9 `video_copy` tests: "HEVC needs the VA-API plugin". Environmental, fail the same on clean main.
- `remote-run` full run: queued behind other lanes for the whole session, so it ended up building
  locally. The first server run with the real code also failed on the missing assets described above.
- Clippy: `cargo clippy -p engine --all-targets -- -D warnings` fails on both main and this branch
  for pre-existing reasons (67 errors, 17 warnings, identical after ignoring line numbers). No new
  findings in audio.rs, esds.rs, adts.rs or audio_aac.rs.

## Known gap (edith_codecs, not this lane)
- **Bare-ADTS HE-AAC plays at the core rate.** Probed: an ADTS HE-AAC file (`ffmpeg -c copy -f adts`)
  opens at 22050 Hz with 2 channels. ec-aac only enables SBR when a config says so (explicit
  signalling), so implicit SBR in ADTS frames is not reconstructed. The rate and the samples are
  consistent, but the output has half the bandwidth. This needs implicit SBR detection in ec-aac.
- HE-AAC v2 (PS) is still refused in mp4 (ec-aac decision D4).
- Matroska HE-AAC rate comes from the CodecPrivate config. Covered by the mkv test.

## Commit
Branch `lane-aacall`, commit `ea4b21f` on base `b7d8ed0`.
