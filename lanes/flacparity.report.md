# lane flacparity: ec-flac level and bit-depth parity with flacenc

Base: main `654717d7`. Branch `lane/flacparity`.
Criterion (DONE.md, ec-flac wave 1, replaces flacenc): encode parity with flacenc at the
compression levels and bit depths edith uses. Gateaudit row: "flacenc level and bit-depth
parity UNKNOWN (`swapflacenc`)".

## What edith used from flacenc
- Source: `git show 0bbb258^:crates/engine/src/export.rs`, `write_flac`:
  `flacenc::config::Encoder::default()` (4096-sample fixed blocks, LPC to order 12,
  Rice partitions to order 6), 16 bits, fixed block size. flacenc 0.5.1.
- Bit depth: the export asked for 16 bits only. Edith's current export
  (`crates/engine/src/export.rs`, `write_flac`) still passes 16. 24-bit is reachable
  through `engine::flac_write` but no export uses it, so it is tested as well.
- Level: flacenc has no level setting. Edith used its default, which is the
  same as `EncoderConfig::default()` in ec-flac.
- ec-flac has no compression-level enum. Its levels are `EncoderConfig` presets, paired
  with the nearest ffmpeg `-compression_level`: L0 fixed (no LPC, partitions to 3) ->
  level 0; default (edith) -> level 5; L8 max (LPC to 32) -> level 8.

## Changes
- New `crates/ec-flac/tests/level_depth_parity.rs`. No source changes were needed.
- Signals: ffmpeg `aevalsrc` generates a sine per channel plus a little noise, 5 s,
  2.0 at 44.1 kHz (16-bit) and 2.0 at 48 kHz (24-bit), and 5.1 at 48 kHz (16-bit and 24-bit).
- Per case and level, the test checks:
  - (a) ffmpeg decodes our stream to the input PCM, byte for byte;
  - (b) STREAMINFO rate, channels, bit depth, total_samples and MD5 are correct
    (MD5 is compared with `md5sum` of the input PCM), and ffprobe agrees on
    rate, channels and bits_per_raw_sample;
  - (c) our size <= 1.05 x ffmpeg's FLAC size at the matching level.
- Reference: ffmpeg's FLAC encoder (`-c:a flac -compression_level N`). The `flac`
  CLI is not installed. The ffmpeg 24-bit reference keeps bits_per_raw_sample=24
  (checked with ffprobe).

## Size table (bytes, 5 s signal)

| case | ec-flac level | ec-flac bytes | ffmpeg -compression_level | ffmpeg bytes | ratio | verdict |
|---|---|---|---|---|---|---|
| 16-bit 2.0 44.1k | ec L0 fixed | 470056 | 0 | 507043 | 0.9271 | PASS |
| 16-bit 2.0 44.1k | ec default (edith) | 427379 | 5 | 447614 | 0.9548 | PASS |
| 16-bit 2.0 44.1k | ec L8 max | 425372 | 8 | 442734 | 0.9608 | PASS |
| 16-bit 5.1 48k | ec L0 fixed | 2414929 | 0 | 2540546 | 0.9506 | PASS |
| 16-bit 5.1 48k | ec default (edith) | 2340854 | 5 | 2359846 | 0.9920 | PASS |
| 16-bit 5.1 48k | ec L8 max | 2335395 | 8 | 2348322 | 0.9945 | PASS |
| 24-bit 2.0 48k | ec L0 fixed | 849357 | 0 | 1008012 | 0.8426 | PASS |
| 24-bit 2.0 48k | ec default (edith) | 788250 | 5 | 883231 | 0.8925 | PASS |
| 24-bit 2.0 48k | ec L8 max | 787801 | 8 | 864080 | 0.9117 | PASS |
| 24-bit 5.1 48k | ec L0 fixed | 3854997 | 0 | 3981180 | 0.9683 | PASS |
| 24-bit 5.1 48k | ec default (edith) | 3780894 | 5 | 3799887 | 0.9950 | PASS |
| 24-bit 5.1 48k | ec L8 max | 3775432 | 8 | 3788362 | 0.9966 | PASS |

All 12 rows are at or below the reference, and the largest ratio is 0.9966 against
the 1.05 margin. The 16-bit default row (427379 B, 2.0 at 44.1 kHz, 5 s) is the edith
configuration.

Earlier direct comparison with flacenc (`lanes/edith-swapflacenc.report.md`, 0.5 s
synthetic, 16-bit stereo 44.1 kHz): flacenc 68198 B, ec-flac 68116 B (-0.12%). The
flacenc 24-bit figures in that report were not sane and are not used here.
flacenc is not in this tree, so it was not re-run.

## Verify (server, `scripts/remote-run.sh flacparity --host auto`, host a)
- `cargo test --release -p ec-flac`: 16 + 2 + 1 + 4 passed, 0 failed. This includes
  `level_depth_parity` (1 passed, 8.0 s).
- `cargo clippy -p ec-flac --all-targets -- -D warnings`: exit 0.
- `rustfmt --check` on the new test file: clean.

## Left
- ec-flac has no compression-level enum, so "levels" here means the three
  `EncoderConfig` presets above, not libFLAC's levels 0-8.
- The reference is ffmpeg's FLAC encoder, not the `flac` CLI. The MD5 check uses
  `md5sum` on the input PCM.
- Edith still exports 16-bit FLAC only. 24-bit export is a separate decision.

## Verdict
- flacenc level and bit-depth parity (ec-flac wave 1): **MET**, 12 of 12 rows PASS.
