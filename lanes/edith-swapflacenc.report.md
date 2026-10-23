# edith lane swapflacenc: FLAC export through ec-flac

Branch `lane-swapflacenc`, commit `cfe7fd9`, base `codecs-integration` (bfcd98c).

## What changed
- New `engine::flac_write::write(out, samples, channels, bits, rate)` calls
  `ec_flac::encode` with `EncoderConfig::default()`. Edith has no FLAC
  compression setting, so nothing maps; the default is the level ec-flac ships.
- `export.rs` `write_flac` now calls it with 16 bits, as before. Edith
  still exports 16-bit FLAC only; 24-bit is reachable through the writer, not
  the export.
- User-visible label changed: `FLAC · SW (flacenc)` to `FLAC · SW (ec-flac)`,
  and `caps.rs` `symphonia / flacenc` to `symphonia / ec-flac`.
- `flacenc` removed from `crates/engine/Cargo.toml` and `Cargo.lock`.
  `cargo tree -p engine -i flacenc --offline` reports "did not match any packages".
- No existing test referenced flacenc directly.
- New `crates/engine/tests/flac_export.rs`: 16-bit stereo, 24-bit stereo and
  24-bit 5.1 through the writer. Each is decoded with ffmpeg (`s32le`) and
  compared sample for sample against the input, shifted to 32 bits. ffprobe
  checks rate, channels and `bits_per_raw_sample`. The STREAMINFO MD5 is
  compared with `md5sum` over the input PCM. `flac -t` is not installed here,
  and ffmpeg does not verify the MD5, so the direct MD5 comparison stands in.

## Verify (run on server via remote-run, EDITH_TREE=~/.cache/wt/edith-swapflacenc)
- `cargo test -p engine --release --test flac_export`: 3 passed, 0 failed.
- `cargo test -p engine --release --no-fail-fast` (full): 54 result blocks,
  failures:
  - 9 `video_copy` tests: known (VA-API HEVC plugin missing).
  - `playback::import_refuses_what_does_not_match`: known.
  - `tonemap::tests::perf_4k_and_1080p`: timing threshold (38.6 ms vs 25 ms in
    the first run; 24.0 ms vs 25 ms in the rerun). Timing on a loaded server,
    unrelated to FLAC or export. Not checked against base.
  - `subtitles::a_project_naming_many_tracks_of_one_file_walks_it_once_and_keeps_its_order`:
    failed once (9042 bytes read vs 705), passed on rerun. Flaky, unrelated.
  - No failure mentions FLAC or export.
- `cargo clippy -p engine --all-targets`: 162 warnings (164 in the earlier
  run before a fix in the new test). None in the files this lane touched
  (`flac_write.rs`, `flac_export.rs`, `caps.rs`, `lib.rs`, and `write_flac`).
  Warnings elsewhere are pre-existing. Not diffed against a codecs-integration
  clippy run.

## Size comparison (report only, not asserted)
A throwaway crate (`flacenc` 0.5.1, default features off, same settings as the
old writer, same synthetic signal):

| case | flacenc | ec-flac |
|---|---|---|
| 16-bit stereo, 44.1 kHz, 0.5 s | 68198 B | 68116 B |
| 24-bit stereo, 48 kHz, 0.5 s | 295920 B | 122107 B |
| 24-bit 5.1, 48 kHz, 0.5 s | 906348 B | 366804 B |

The flacenc 24-bit figures are larger than the raw PCM (144000 B and 432000 B),
so they are suspect: the flacenc 24-bit path in this harness did not produce
a sane size and was not investigated further. The 16-bit numbers agree
closely. The 24-bit comparison should not be relied on.

## Left
- Edith's export still writes 16-bit FLAC only. 24-bit export is a separate decision.
- Export still holds all samples in memory, as before.
- `export.rs` was already over the 5,000-line limit before this lane (about
  6,000 lines). The change shrinks it; splitting it was not part of this lane.
