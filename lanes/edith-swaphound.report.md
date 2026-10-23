# edith lane `swaphound`: hound replaced by ec-riff WavWriter, RF64 added

Goal (DONE.md, wave 1): replace `hound` with ec-riff's `WavWriter` in edith's WAV export, and close the RF64 gap for exports over 4 GiB.

Branches
- edith_codecs: `lane/swaphound` at `379ebb51` (base `e0fad43b`)
- edith: `lane-swaphound` at `63a969c3` (base `b7d8ed0`)

## What changed

### ec-riff (edith_codecs)
- `WavOptions { expected_frames, rf64_threshold }` and `WavWriter::with_options` / `create_with_options`. The container is chosen before the first sample: RF64 when `expected_frames * block_align >= rf64_threshold` (default `RF64_THRESHOLD = 0xFFFF_F000`). `new` and `create` are unchanged and still write RIFF.
- RF64 header: `RF64` + sentinel size, `ds64` chunk (riffSize, dataSize, sampleCount as u64, tableLength 0), then the usual fmt/fact/data. The 32-bit data field stays at `0xFFFFFFFF`. The fact field saturates at u32::MAX.
- A RIFF writer that grows past 4 GiB is still refused at finalize, and the error now names the fix (`expected_frames`).
- `WavReader` accepts `RF64` and `BW64` and takes the data size from ds64 when the data field is the sentinel.
- Tests: `crates/ec-riff/tests/rf64.rs`. Six run in the normal suite: exact ds64 sizes with the threshold injected, RF64 vs RIFF sample equality across depths and layouts, odd-length pad byte, extensible fact field, RIFF unchanged under the threshold, and a pass-through RIFF check. One is `#[ignore]`: it writes 4.1 GiB under `TMPDIR` and reads the header back.

### edith (`lane-swaphound`)
- `crates/engine/src/export.rs` `write_wav`: `ec_riff::WavWriter::create_with_options`, with `expected_frames` set from the sample count. Same spec as before: 16-bit int, `audio.channels`, `audio.sample_rate`.
- Labels: `"PCM · SW (hound)"` is now `"PCM · SW (ec-riff)"`, and `caps.rs` `symphonia / hound` is now `symphonia / ec-riff`. No test asserted on the old strings.
- Tests: `tests/lanes.rs`, `tests/speed.rs` (x2) and `tests/audio_export.rs` read with `ec_riff::WavReader::read_all_i32()` in place of hound's `samples::<i16>()`. The values are the same for 16-bit.
- New test `the_wav_export_is_ffprobe_clean_and_decodes_to_the_same_pcm`: ffprobe reports `pcm_s16le`, the channel count, the rate and a duration within 1 ms of frames/rate. `ffmpeg -i out.wav -f s16le -` produces exactly the samples ec-riff read back. It skips when ffmpeg is absent.
- `hound` removed from `crates/engine/Cargo.toml` (it was only in `[dependencies]`), `ec-riff` added, Cargo.lock refreshed. `cargo tree -p engine -i hound` finds nothing.

## Byte comparison: hound vs ec-riff (same 16-bit input)

Written by a throwaway crate outside both repos (`~/.cache/wt/swaphound-cmp`), built locally.

| channels | frames | hound bytes | ec-riff bytes | identical |
|---|---|---|---|---|
| 1 | 0, 1, 333, 4800 | 44, 46, 710, 9644 | same | yes |
| 2 | 0, 1, 333, 4800 | 44, 48, 1376, 19244 | same | yes |
| 6 | 0, 1, 333, 4800 | 68, 80, 4064, 57668 | +12 bytes each | no |
| 8 | 0, 1, 333, 4800 | 68, 84, 5396, 76868 | +12 bytes each | no |

Mono and stereo are byte-for-byte identical. For 3 or more channels ec-riff writes `WAVE_FORMAT_EXTENSIBLE` (40-byte fmt, channel mask, `fact` chunk); hound writes a bare `WAVE_FORMAT_PCM` 16-byte fmt. ec-riff is the more correct one. For more than two channels the spec says the bare PCM tag does not define the speaker layout, and EXTENSIBLE carries the mask. The extra bytes are the 12 bytes of extensible fmt growth plus fact. Both decode to the same samples.

## Verify results

### ec-riff (on the server, lane tree `lane/swaphound`)
- `cargo test -p ec-riff`: `tests/wav.rs` 11 passed; `tests/rf64.rs` 6 passed, 1 ignored; no lib unit tests changed.
- `cargo test -p ec-riff --test rf64 -- --include-ignored writes_past_four_gib_as_rf64`: passed (146 s; header RF64, riffSize and dataSize match the file, duration matches the frame count).
- `cargo clippy -p ec-riff --all-targets -- -D warnings`: fails with 3 warnings, identical to main's (`chunks_exact` in avi.rs:545 and read.rs:148 (main: 134), `needless_question_mark` in avi.rs:634). Those lines are not in this lane's code. No new warnings, and the new test files are clean.
- Caveat: the first clippy run also flagged two unused `mut` in my test file. Both are fixed.

### engine (`cargo test -p engine --release --no-fail-fast` on the server, edith lane tree with ec-riff from the lane)
- Lane: 820 passed, 13 failed, 56 ignored. Main (baseline, same server setup): 821 passed, 11 failed, 56 ignored. The lane has one more test (the new ffprobe test).
- The 11 failures present on both sides are all environmental on this host: HEVC decode needs `libengine_hw.so` (VA-API plugin), which the server doesn't have (video_copy and friends), plus `import_refuses_what_does_not_match` and `tonemap::perf_4k_and_1080p` (a 26 ms/frame timing budget under load).
- The 2 extra lane failures were timing or load flakes: `bitrate::no_decoder_is_opened` ("probe took 63 ms") and `subtitles::a_project_naming_many_tracks...` (a read-count check). Both pass when rerun on the lane (`lane-recheck.log`).
- Every WAV test passes on the lane: `exports_the_timeline_as_a_wav`, `the_wav_export_is_ffprobe_clean_and_decodes_to_the_same_pcm`, `a_51_ac3_source_round_trips_through_a_wav`, `the_sound_is_written_at_the_rate_that_was_asked_for`, `a_wav_export_of_a_2x_*`, and the lanes and speed tests that now read with ec-riff.
- `cargo clippy -p engine --all-targets -- -D warnings`: 69 error lines on both main and the lane, identical lint sets. Engine was not clippy-clean on this toolchain before this lane, and the lane adds none.

## Gaps and notes
- RF64 end-to-end at the export level is not run: a real export over 4 GiB needs ~8 GB of RAM (`write_wav` collects all samples as i32). The header path is covered by ec-riff's tests, and `write_wav` passes the frame count so the RF64 header is chosen. I did not run an edith export at that size.
- server setup: the edith test media (`assets/`, gitignored, 28 MB) is not synced by remote-run (it excludes `/assets/`), and the host has no copy. I copied it once into `~/lanes/edith-swaphound/edith/assets` and `~/lanes/edith-base/edith/assets` on both hosts. `--clean` removes those.
- Local runs: the lane worktree has no edith `assets/` by default, so I copied it into `~/.cache/wt/edith-swaphound` (not committed; gitignored).
- Trailer: the commits end with `Co-Authored-By: Claude Haiku 5.5`, following the attribution reminder. The lane docs say Opus 5.5.
- `rustfmt`: audio_export.rs was formatted (it was clean on main). lanes.rs, speed.rs and ec-riff's lib.rs/avi.rs were already unformatted on main, so I did not reformat them.
- Logs: `~/.cache/wt/lane-final.log`, `~/.cache/wt/base-final.log`, `~/.cache/wt/riff-b.log`, `~/.cache/wt/lane-recheck.log`.
