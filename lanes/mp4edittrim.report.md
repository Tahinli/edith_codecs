# Lane mp4edittrim: audio edit duration in Mp4Muxer

## Goal
`Mp4Muxer::set_audio_delay` writes only the priming `media_time`; the edit runs to the end of
the samples. edith copies AAC into an export and needs the audio edit trimmed to the picture's
length, as ffmpeg and old edith did (the copied sound ran about 0.017 s past the picture).

Base: `f5eb855b` (main). Branch `lane/mp4edittrim`.

## API edith should call

```rust
muxer.set_edit(audio_stream, priming_samples, picture_samples)?;
```

- `audio_stream`: the `u32` returned by `add_stream` for the AAC track.
- `priming_samples: u32`: the encoder priming in the track's own timescale (the sample rate,
  1024 for AAC-LC). This is the same value `set_audio_delay` takes.
- `picture_samples: u32`: the picture's length in the same timescale, that is
  `round(picture_seconds * sample_rate)`. No conversion inside the muxer.
- Both values are track ticks. The muxer rounds the segment up to the 1000 Hz movie clock.
- `set_edit` replaces `set_audio_delay` for that stream. Call one or the other, not both.
  Its `media_time` is the shift, so pass the priming there.

## Changes
- `crates/ec-mp4/src/mux.rs`
  - `Track.edit: Option<(u32, u32)>` holds `(media_time, duration)`.
  - `pub fn set_edit(&mut self, stream: u32, media_time: u32, duration: u32) -> Result<()>`.
  - At `finish`, a set edit writes one `edts/elst` entry: `segment_duration` in the movie
    timescale (rounded up), `media_time` in the track timescale, rate 1.0.
  - `tkhd` duration and the movie `mvhd` duration use the edited span. `mvhd` is the maximum of
    every track's presentation length, so an unedited track is unchanged.
  - Without a call nothing changes. Only the new branches are taken, and the default path
    still runs the same code.
- `crates/ec-mp4/tests/edit_trim.rs` (new)
  - `default_aac_priming_output_is_byte_identical`: MD5 of the AAC priming output
    (`set_audio_delay(1024)`, 47 frames), pinned as `2ba0fd44a7dfa3b69cb60d7aceb71753`. The
    value was captured on the unchanged tree before `set_edit` was written, and matches after.
  - `set_edit_trims_the_audio_edit_to_its_stated_length`: priming 1024, picture 40000 samples
    against 48128 samples of media. Checks `tkhd` and `mvhd` both equal 834 ms, `mvhd`
    timescale 1000, the crate's demuxer reads one edit with `media_time` 1024 and
    `segment_duration` 834, and ffprobe `start_time` is 0 with stream duration within one AAC
    frame of the picture.

No changes to `demux.rs`. No changes to the `ec-core` API. `cargo fmt` was run on the crate and
its unrelated reformatting was reverted, so the diff is limited to `mux.rs` and the new test.

## Verify (server, `scripts/remote-run.sh mp4edittrim --host auto`, host c)
- Baseline MD5 on unchanged code: `2ba0fd44a7dfa3b69cb60d7aceb71753`. After the change: same.
- `cargo test --release -p ec-mp4`: all green.
  - lib 26 passed
  - edit_trim 2 passed
  - edith_parity 2 passed
  - fuzz_regressions 1 passed
  - mp4 16 passed, 1 ignored (`the_real_library_demuxes_to_eof`, pre-existing)
  - mux_entries 10 passed
  - rotation 3 passed
- `cargo clippy -p ec-mp4 --all-targets -- -D warnings`: clean, exit 0.
- ffprobe on the new test output: `start_time 0`, `duration 0.834` for a `0.8333` s picture.
  The movie clock rounds the segment up to 834 ms.

## Notes and what is left
- Not wired in edith. That is the edith call site above.
- `set_edit` overrides a reorder-delay or Opus pre-skip shift on the same track. It is meant
  for audio tracks. Video edits are not covered by a test.
- The 1000 Hz movie clock gives up to 1 ms of rounding on `segment_duration`. The sample-
  accurate value is still in the track's `media_time`.
- No test on a multi-track file (edited audio next to video). The `mvhd` maximum rule is
  implemented but only the audio-only case is tested.
