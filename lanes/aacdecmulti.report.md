# lane aacdecmulti: ec-aac multichannel parity against ffmpeg

Base: `f5eb855b` (main), branch `lane/aacdecmulti`.

## Goal

DONE.md ec-aac decode criterion: AAC-LC mono, 5.1 and 7.1, raw and ADTS, must reach
per-channel correlation >= 0.999 against ffmpeg's decode.

## Changes

- `crates/ec-aac/tests/multichannel_parity.rs` (new). Generates fixtures at test time with
  ffmpeg (no media committed): an `aevalsrc` multitone with a distinct frequency per
  channel, encoded to ADTS (`.aac`) and to mp4 (`.m4a`, raw access units plus ASC). Each
  fixture is decoded by ec-aac and by ffmpeg (f32le, same channel count), aligned on the
  best integer lag, then each channel's correlation must be >= 0.999. For 5.1 and 7.1 the
  channel order is also checked (our channel k must correlate best with ffmpeg channel k).
  `EC_AAC_PARITY_DEBUG=1` prints the RMS per channel and the full correlation matrix.
- `crates/ec-aac/src/config.rs`: program_config_element is parsed into per-element
  speakers (`Speaker`, `PceElement`). Front runs centre-out (SCE = FC, outermost pair =
  FL/FR, inner pair = FLC/FRC), side pair = SL/SR, back pair = BL/BR, back SCE = BC, LFE.
  `AudioSpecificConfig` gains `pce: Vec<PceElement>` (set when channelConfiguration is 0).
- `crates/ec-aac/src/decode.rs`: raw_data_block records each plane's speaker when the PCE
  names every element; `plane_order` returns the output order in FFmpeg mask-bit order
  (FL FR FC LFE BL BR FLC FRC BC SL SR). Without a PCE, the old channel-config order is used.
- `crates/ec-aac/src/lib.rs`: decode uses `plane_order` when present.
- Struct literal updates for the new `pce` field: `examples/adts_to_pcm.rs`,
  `examples/wrap_sbr.rs`, `tests/sbr_real_library/chunk_b.rs`.

## Root cause found

ec-aac ignored the PCE and used bitstream element order. The fdk 7.1 stream signals
channel_configuration 0 with a PCE (front: SCE, CPE, CPE; back: CPE; LFE), so the
second front pair is FLC/FRC, not SL/SR. ffmpeg outputs in the `7.1(wide)` order, and the
m4a copy of the same stream keeps the PCE only in the ASC, which the decoder also needs.
The native ffmpeg `aac` encoder writes channel_configuration 7 and was already correct.

## Verify

Run on host a via `scripts/remote-run.sh aacdecmulti --host a`:

- `cargo test --release -p ec-aac --test multichannel_parity -- --nocapture`: PASS
  (16 cases: 4 layouts x 2 containers x 2 encoders). Every per-channel correlation is 1.000000.
- `cargo clippy -p ec-aac --all-targets -- -D warnings`: clean.
- `cargo test --release -p ec-aac`: all binaries pass except
  `sbr_real_library::chunk_a::full_chain_low_band_matches_own_core`. It fails identically on
  base `f5eb855b` (checked with the changes stashed). It reads `/home/user/Music/reeln.mp4`, a machine-local file, and is unrelated to PCE.

## Correlation table (per channel, lag from the search)

| layout | container | encoder (profile) | channels | correlation per channel |
|---|---|---|---|---|
| mono | adts | aac (AAC-LC) | 1 | 1.000000 |
| mono | m4a | aac (AAC-LC) | 1 | 1.000000 |
| stereo | adts | aac (AAC-LC) | 2 | 1.000000, 1.000000 |
| stereo | m4a | aac (AAC-LC) | 2 | 1.000000, 1.000000 |
| 5.1 | adts | aac (AAC-LC) | 6 | 1.000000 x6 |
| 5.1 | m4a | aac (AAC-LC) | 6 | 1.000000 x6 |
| 7.1 | adts | aac (AAC-LC) | 8 | 1.000000 x8 |
| 7.1 | m4a | aac (AAC-LC) | 8 | 1.000000 x8 |
| mono | adts | libfdk_aac (AAC-LC) | 1 | 1.000000 |
| mono | m4a | libfdk_aac (AAC-LC) | 1 | 1.000000 |
| stereo | adts | libfdk_aac (AAC-LC) | 2 | 1.000000, 1.000000 |
| stereo | m4a | libfdk_aac (AAC-LC) | 2 | 1.000000, 1.000000 |
| 5.1 | adts | libfdk_aac (AAC-LC) | 6 | 1.000000 x6 |
| 5.1 | m4a | libfdk_aac (AAC-LC) | 6 | 1.000000 x6 |
| 7.1 | adts | libfdk_aac (AAC-LC, PCE) | 8 | 1.000000 x8 |
| 7.1 | m4a | libfdk_aac (AAC-LC, PCE in ASC) | 8 | 1.000000 x8 |

Before the fix, the fdk 7.1 rows had channels 0,1,6,7 wrong (0.0 against ffmpeg, with
the front pairs swapped). Channel order for 5.1 and 7.1 matches ffmpeg in every row.

## Not covered, and why

- HE-AAC v1/v2: this ffmpeg's libfdk_aac fails with "Unable to set the AOT 5/29" for every
  profile and rate, and the native `aac` encoder has no SBR. HE fixtures were not
  generated. The existing HE-AAC files under `crates/ec-aac/tests/data` are stereo only
  and already covered by other tests.
- Channel configuration 12: no encoder available here emits it, and
  `channels_for_config` still returns 0 for 12. Unverified and unchanged.
- PCE in the ASC is handled for the mp4 path. A PCE that appears only inside a raw block
  with no ASC is handled too, but only the in-band one is exercised by the matrix.
- Lag search is an integer search over +/-4096 samples. A steady tone repeats at its
  period, so the search scores the near-tied candidates over the full overlap.
