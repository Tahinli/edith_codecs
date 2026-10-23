# Lane aacps: HE-AACv2 Parametric Stereo

## Goal
Decode HE-AACv2 PS streams (ISO/IEC 14496-3 4.6.18 Parametric Stereo) in `crates/ec-aac`:
parser, hybrid analysis, decorrelation, stereo processing, hybrid synthesis, wired through
SBR into the decoder. Malformed PS falls back to mono duplicated on both channels.

Base sha: `7b06ac32`. Branch: `lane/aacps`.

## Status
PARTIAL. PS decodes to two channels at the extension rate and matches FFmpeg on the music
fixtures (corr >= 0.9997). The pure-tone (sine) fixtures do NOT match: at zero lag the
correlation is about 0.70 while RMS levels agree. They are listed as KNOWN_INEXACT in
`crates/ec-aac/tests/ps_oracle.rs` with a 0.69 floor. The cause is not isolated.

## Changes (commits)
- `f9241bb6` WIP PS parser and DSP draft (`ps_data.rs`, `ps_huff.rs`, `ps_dsp.rs`, `scripts/aac-tables/gen-ps-huff.py`).
- `bfe5d562` Fixtures: `crates/ec-aac/tests/data/ps_{44100,48000}_{sine,music}.aac` (ADTS, implicit HE-AACv2), generator `scripts/aac-tables/make-ps-fixtures.sh` (fdk-aac Fedora RPM reference, no root).
- `56189f42` Declare `ps_data`, `ps_dsp`, `ps_huff` modules.
- `44a0b9fd` Port PS DSP from FFmpeg `aacps.c` / `aacpsdsp_template.c` / `aacps_tablegen.h`.
- `54ea479c` Wire PS through the SBR extension, SBR chain and decoder (`sbr_payload.rs`, `sbr_chain.rs`, `decode.rs`, `lib.rs`).
- `cf70b30d` Oracle assertion change (see below).
- `ed7f9600` PS correlation test vs FFmpeg; clippy lint fixes in PS and SBR chain.
- `e1e6c1c3` Fuzz-found overflow fixes in `decode.rs` (escaped FIL count) and `sbr_payload.rs` (`Grid::new` pointer subtraction).

## Assertion change disclosed
`tests/oracle.rs`, `sbr_is_reported_not_silently_upsampled`: the PS configuration previously
asserted `SbrSupport::CoreOnly`. It now asserts `SbrSupport::V1` and
`output_sample_rate() == Some(48_000)`, since PS is now reconstructed. The `CoreOnly` variant
is kept but no longer produced for PS.

## Commands and results
- `scripts/remote-run.sh aacps -- cargo check -p ec-aac --all-targets`: clean, no warnings.
- `scripts/remote-run.sh aacps -- cargo clippy -p ec-aac --all-targets -- -D warnings`: clean (after fixing 11 lints in sbr_chain.rs and PS modules, then 1 in the test file).
- `scripts/remote-run.sh aacps -- cargo test -p ec-aac --release --no-fail-fast`: all pass; 1 pre-existing ignored test (13 passed, 1 ignored in that binary). `ps_oracle` passes with the KNOWN_INEXACT list.
- Fuzz (`cargo +nightly fuzz run <target> -- -max_total_time=60`, from `crates/ec-aac`):
  - `decode_adts` first run: crash, `decode.rs:882` subtract overflow. Fixed in `e1e6c1c3`. Rerun after fix: 71111 runs in 61 s, no panic.
  - `decode_raw` first run: crash, `sbr_payload.rs:110` subtract overflow. Fixed in `e1e6c1c3`. Rerun after fix: 83034 runs in 61 s, no panic.
  - No PS-specific fuzz target was added; PS is reached through the existing ADTS and raw targets only.

## Per-fixture / per-channel correlation vs FFmpeg (zero lag, full overlap)
Reference: system FFmpeg native AAC decoder, `-ac 2`, f32. FFmpeg reports the ADTS fixtures as stereo HE-AACv2 at the extension rate. Both sides have identical frame counts.

| fixture | ch | ours frames | ffmpeg frames | corr@0 | bounded-search lag / corr | status |
|---|---|---|---|---|---|---|
| ps_44100_sine | 0 | 96256 | 96256 | 0.708783 | -3107 / 0.697531 | KNOWN_INEXACT |
| ps_44100_sine | 1 | 96256 | 96256 | 0.703701 | -902 / -0.701001 | KNOWN_INEXACT |
| ps_44100_music | 0 | 96256 | 96256 | 0.999922 | 0 / 0.999922 | pass (>= 0.999) |
| ps_44100_music | 1 | 96256 | 96256 | 0.999993 | 0 / 0.999993 | pass |
| ps_48000_sine | 0 | 104448 | 104448 | 0.699835 | -109 / 0.706755 | KNOWN_INEXACT |
| ps_48000_sine | 1 | 104448 | 104448 | 0.716376 | -2400 / 0.702097 | KNOWN_INEXACT |
| ps_48000_music | 0 | 104448 | 104448 | 0.999702 | 0 / 0.999702 | pass |
| ps_48000_music | 1 | 104448 | 104448 | 0.999928 | 0 / 0.999928 | pass |

The bounded lag search picks aliases of the tone period on the sine fixtures, so it is
reported for information only. The primary check is zero lag.

Sine RMS (ours vs FFmpeg, 44.1 kHz): 0.08455/0.08468 vs 0.08533/0.08458. Levels match, so the
error is phase-like on tonal content, not a gain error.

## Investigation of the sine mismatch
Checked against the FFmpeg source and found to match: the `hybrid2_re` index pairs and
reverse flag, the 20-mode `hybrid6_cx` row mapping, and the filter tables (`make_filters`
vs `make_filters_from_proto`).
Not yet checked: `stereo_processing` (IPD/OPD and the sign flip for bands 9..13 in 34-mode
and bands <= 1 in 20-mode), `decorrelation` (all-pass delays and transient gain), and
`hybrid_synthesis`.

## What is left
1. Isolate the tonal phase error: test with decorrelation disabled, and check the
   stereo_processing flip and the IPD/OPD path against `aacps.c`.
2. Remove the KNOWN_INEXACT entries once the sine fixtures reach 0.999.
3. `ps_data`/`ps_dsp` have no unit tests for the hybrid filter bank in isolation.
4. The four `.m4a` copies of the fixtures are untracked in the worktree and not committed;
   the tests read only the `.aac` files. The `.m4a` files are not needed for the test.
5. No PS-specific fuzz target yet.
