# Lane aacimplicit: implicit SBR signalling in ec-aac

Base: main `fc963179`. Branch `lane/aacimplicit`.

## Goal
A stream whose AudioSpecificConfig or ADTS header says plain AAC-LC, but whose raw_data_block
carries an SBR extension payload, is HE-AAC by implicit signalling (ISO/IEC 14496-3 1.6.5.2).
ffmpeg, FAAD and the ISO reference decode it at twice the core rate. ec-aac used to output the
core rate only, at half bandwidth. edith hit this with bare ADTS HE-AAC files
(`lanes/edith-aacall.report.md`, "Known gap").

## What changed
- `crates/ec-aac/src/decode.rs` (`BlockDecoder`):
  - New fields `implicit_core` (core rate an implicit payload may double, `None` when off) and
    `implicit_on` (switched on by a payload).
  - `set_implicit_core(core_rate)`: arms detection only when no SBR is set, and only for a core
    rate of at most 24000 Hz (doubled rate <= 48000). Above that the SBR payload is skipped and
    the output stays at the core rate.
  - In the FIL branch: on the first SBR extension (type 13/14) that follows an SCE/CPE, the SBR
    chain is created at 2x the core rate for that element. If its payload does not parse, the
    chain is dropped again, so a non-SBR fill never switches the rate.
  - `implicit_sbr()` accessor.
- `crates/ec-aac/src/lib.rs` (`AacDecoder`):
  - `with_config`: a plain-LC config (no SBR, no PS) arms implicit detection at its sample rate.
  - ADTS decode path: arms implicit detection from the first frame's header rate.
  - `sbr_implicit()` (new, public): true once an implicit payload has switched SBR on.
  - `sbr_support()`: returns `SbrSupport::V1` once implicit SBR is on, so the stream reports
    the SBR it actually decodes. Explicit behaviour is unchanged.
  - `output_sample_rate()`: now also answers for ADTS-only decoders, so it is `None` before the
    first frame and the core rate after it. It is the doubled rate once implicit SBR is on.
    Explicit SBR and LC configs return the same values as before.
  - Doc comments updated to say that callers should re-read the rate after each packet.
- `crates/ec-aac/tests/implicit_sbr.rs` (new): 3 tests (see below).
- `crates/ec-aac/tests/data/heaac_44100_48k_implicit_2s.aac` (new, 13.6 KB): 2 s of
  `heaac_44100_48k.m4a` (HE-AAC v1, stereo, 44100 Hz) remuxed to bare ADTS with
  `ffmpeg -t 2 -c copy -f adts`. ffprobe reads it as HE-AAC at 44100 Hz. Its ADTS headers say
  LC at 22050 Hz, so the SBR signalling is implicit.

## Behaviour notes and limits
- The switch happens in the frame that first carries the SBR payload, so that frame is already
  output at the doubled rate. This matches ffmpeg, which decodes the first frame with SBR.
- Frames before the first SBR payload would be at the core rate. In practice every SBR frame
  carries its payload, so this does not arise in the fixture.
- Downsampled SBR (core above 24 kHz with SBR present): not implemented. The SBR chain has no
  downsampled mode, so the SBR payload is skipped and the output stays at the core rate. An
  implicit stream at 32/44.1/48 kHz core would therefore still be at the core rate, half the
  bandwidth. This is documented, not silently claimed.
- Once an implicit payload has enabled SBR, a later frame with no SBR payload uses the last
  payload's data (`apply_last`). This is the same as explicit streams.
- The detection is keyed on extension type 13/14 at the start of a FIL right after an SCE/CPE.
  It is not a full syntax check, so a plain LC stream whose fill payload happens to start with
  those bits and parses as SBR would be switched. Not seen in any fixture here.

## Tests
- `cargo test -p ec-aac --release --no-fail-fast` on the server (`remote-run`): all green.
  - lib unit tests: 43 passed, 0 failed.
  - `tests/implicit_sbr.rs` (new): 3 passed.
    - `implicit_he_aac_adts_switches_to_double_rate`: no rate before frame 1; SBR implicit; rate
      44100 for every frame; stereo; about 2 s; both channels non-silent.
    - `implicit_he_aac_adts_matches_ffmpeg_per_channel`: correlation with
      `ffmpeg -i X -f f32le -` >= 0.999 per channel.
    - `plain_aac_lc_adts_is_unchanged_by_implicit_detection`: `fixtures/audio/aac-adts-stereo-44100.aac`
      stays at 44100 with no implicit switch (`NotSignalled`) and correlation >= 0.999 with ffmpeg.
  - `oracle.rs`: 13 passed, 1 ignored (`real_library_tracks_match_ffmpeg`, pre-existing ignore for
    the personal library).
  - `sbr_real_library.rs`: 16 passed. `no_panic.rs`: 2 passed. `reelh_per_frame.rs`: 1 passed.
    `obs_real_content.rs`: 1 passed.
- `cargo clippy -p ec-aac --all-targets -- -D warnings` on the server: clean, exit 0.
- `cargo check --workspace --all-targets` on the server: see the final message for the result.
- Caveat: test output was not captured with `--nocapture`, so the ffmpeg-correlation tests
  reported ok without a visible SKIP. I did not verify that ffmpeg was on the server PATH, so the
  ffmpeg comparison is only confirmed where ffmpeg runs.

## API change (for edith)
- `AacDecoder::sbr_implicit() -> bool` (new).
- `AacDecoder::sbr_support()` returns `V1` for an implicit stream once switched.
- `AacDecoder::output_sample_rate()` changes after the first implicit frame: the core rate
  becomes 2x. For an ADTS decoder it is `None` until the first frame.
- edith impact: `crates/engine/src/audio.rs` takes the track rate from `aac_output_rate` at open,
  which is still the core rate for bare ADTS HE-AAC. Edith needs to read the rate again after
  the first decoded frame (or the rate must be handled in the packet path) before this fixes
  edith playback. This lane does not change edith.

## Not done
- Downsampled SBR for core above 24 kHz.
- Edith-side rate refresh after the first implicit frame.
