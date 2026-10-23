# edith lane truehd: Matroska TrueHD and MLP through ec-truehd

Branch `lane-truehd`, commit `bb622ae` on base `codecs-integration` (bfcd98c).

## Goal
DONE.md D6: edith decodes Dolby TrueHD / MLP audio from Matroska through
`ec-truehd`. Before this, the track fell to symphonia (no decoder) and played silent.

## What changed
- `crates/engine/Cargo.toml`: path dep `ec-truehd`. `Cargo.lock` gains its entry.
- `crates/engine/src/demux.rs`: `MkvAudio` maps `A_TRUEHD` -> `truehd`, `A_MLP` -> `mlp`.
- `crates/engine/src/audio.rs`:
  - `MkvTruehdTrack` and `run_mkv_truehd`, modelled on the Matroska AC-3 track.
  - Rate, layout and access-unit length come from the first decodable frame, which
    the decoder builds from the major sync.
  - Samples are S32 from `ec-truehd`, converted to f32 by dividing by 2^31 (exact).
  - Presentation: the decoder's default (highest: 7.1, else 5.1, else stereo).
    The worker folds it to the stereo pair with the existing `downmix`.
  - Sample positions are `block_index * au_samples`, not summed, so a block the
    decoder has not yet emitted cannot shift later audio.
  - Pre-roll of 40 blocks for seeks.
  - Opening a TrueHD track never falls through to symphonia. An open error (Atmos)
    is returned, so the user gets a named refusal.
  - Copy into mp4 refuses TrueHD by name (`uncopyable`); export re-encodes.
  - Stream listing, stream order and `unsupported()` know the TrueHD track.
- `crates/engine/tests/audio_truehd.rs` (new): fixtures made at test time by
  `ffmpeg -c:a truehd -strict -2` (2 ch and 6 ch), each sine 440 Hz, 2 s.
  - stereo opens at 44100 / 2 channels;
  - 5.1 opens at 44100 / 2 channels (the folded pair);
  - stereo decode equals ffmpeg `-f s32le` sample for sample (exact, 0 mismatches);
  - 5.1 timeline channels correlate > 0.99 with ffmpeg `-ac 2` per channel.

## Presentation choice
The decoder's default is used: the highest presentation in the stream. A 7.1
stream is decoded as 7.1 and folded to stereo at the worker, like other
multichannel sources. The timeline never carries more than a pair.

## Atmos
`ec-truehd` refuses a 4th (object/16-channel) substream by name, and returns
`Unsupported` on the access unit. The decoder does not offer a 7.1 or 5.1
presentation for such a stream, so none is decoded. edith refuses the open
by name. Playing the PCM presentation under an Atmos track would need an
`ec-truehd` change (skip substream 4), which is not in this lane.

## mp4
edith does not demux TrueHD in mp4 (no `mha1`/`mhm1` in the mp4 path), so
nothing changed there.

## Verify
Run from `/home/user/Documents/Code/Rust/edith_codecs` with
`EDITH_TREE=~/.cache/wt/edith-truehd scripts/remote-run.sh edith-truehd --with-edith -- ...`.

- `cargo test -p engine --release --test audio_truehd`: 4 passed, 0 failed
  (final rerun after the clippy fix, too).
- `cargo test -p engine --release --no-fail-fast` (first run): 849 passed, 11 failed.
  All 11 are known or environmental:
  - 9 `video_copy` tests (known, VA-API HEVC plugin);
  - `playback::import_refuses_what_does_not_match` (known);
  - `tonemap::tests::perf_4k_and_1080p` (timing test, load 11-19 on the server; the
    coordinator classed it environmental, not rerun).
- `cargo clippy -p engine --all-targets`: exit 0, 165 warnings. The first run had
  167, two of them mine (`run_mkv_truehd` argument count, which I allowed with a
  reason to match `run_mkv_ac3`; and `chunks_exact`, switched to `as_chunks`). The
  baseline was not run on codecs-integration, so "no new warnings" is inferred
  (167 - 2 mine = 165), not measured.

## Left
- Atmos (see above) needs `ec-truehd` to skip substream 4.
- mp4 TrueHD is not demuxed.
- The 5.1 fold is verified by correlation, not sample-exact, since the stereo
  fold coefficients are edith's.
