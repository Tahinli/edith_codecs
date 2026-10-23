# lane aacsbrregress: report

## Goal
Fix the local regression where `sbr_real_library::chunk_a::full_chain_low_band_matches_own_core`
failed on a real-library HE-AAC mp4 (`ch0: measured best lag -53001, corr 0.016086 < 0.99`).

Base sha: `7d8711af` (main). Branch: `lane/aacsbrregress`.

## Root cause
The failure is a harness regression, not a decoder regression.

- `our_decode_core_only` (tests/sbr_real_library/helpers.rs) builds the config with
  `sbr_present = false` and calls `AacDecoder::with_config`. Since commit `7ccf6c24`
  (lane/aacimplicit, merged to main as `a1aec0e7`), `with_config` arms *implicit* SBR for any
  non-PS config. The stream's SBR fill payloads therefore switched SBR on, and the "core-only"
  PCM was the full chain at 44.1 kHz (sample count 1001472, the same as the full-chain decode),
  while the helper still labelled it 22050 Hz.
- The "core" leg was therefore the full chain. Its low band was compared against the naive 2x
  upsample of itself at the wrong rate, and the correlation is noise.
- Evidence:
  - a probe on the helper printed `implicit=true support=V1 len0=1001472` for the core-only decode;
  - before the fix, `core_only_matches_reference` scored lag -3816 / corr 0.0026 (ch0) against
    ffmpeg's core, which is noise;
  - the full chain is correct: `sbr_real_library_matches_reference` scores full-band
    correlation 0.999864 at lag 0 vs ffmpeg, below-5 kHz 0.999988, and passes both before and after.
- After the fix, core-only vs ffmpeg's core gives lag 481, corr 0.999845 (ch0) and 0.999779
  (ch1) on the real-library mp4, matching the number documented in the test's comment. A second
  real-library HE-AAC file, not named here, gives 481 / 0.999999.

## Bisect
`git bisect run` over main with a test-only predicate (`cargo test --release -p ec-aac
--test sbr_real_library full_chain_low_band_matches_own_core -- --exact`), shared
`CARGO_TARGET_DIR=~/.cache/tgt-aacsbrregress`.

- Bisect endpoints: bad `7d8711af`, good `74edcdab` (not directly tested).
- Result: first bad commit `7ccf6c24` ("lane/aacimplicit: implicit SBR signalling in ec-aac").
  Its parent `d4e45d31` was checked manually: test passes (`ok. 1 passed`). `7ccf6c24` fails.
  Main merged this as `a1aec0e7`.
- Caveat: the bisect ran the test as each commit itself defines it. Earlier commits in the range
  had a different test layout (a flat `sbr_real_library.rs`), so overlaying this lane's test
  tree was not possible: it failed to compile against older `AudioSpecificConfig`
  (the `pce` field from aacdecmulti). The aacgates2 merge `e51e3a34` was checked too, and is
  bad only because it is after `a1aec0e7`.
- The aacdecmulti (PCE), aacencbar, mp4demux, fuzz1hcontainers and imagebudget merges were not
  the culprit: the bisect's first bad commit is the implicit-SBR lane commit, which predates all of
  them.

## Changes
- `crates/ec-aac/src/decode.rs`: `BlockDecoder` gets an `implicit_off` flag and
  `disable_implicit_sbr()`. `set_implicit_core` respects the flag. Default behaviour is
  unchanged: implicit SBR still arms for plain-LC streams that carry SBR payloads.
- `crates/ec-aac/src/lib.rs`: new public `AacDecoder::core_only(self) -> AacDecoder`. It drops
  any explicit SBR or PS from the config, disables implicit SBR, and clears the SBR chain. Output
  stays at the core rate and SBR payloads are skipped.
- `crates/ec-aac/tests/sbr_real_library/helpers.rs`: `our_decode_core_only` uses
  `.core_only()`. This is the actual fix to the failing test's core leg.
- `crates/ec-aac/tests/implicit_sbr.rs`: new regression test
  `core_only_decode_of_implicit_he_aac_stays_at_core_rate`. It decodes the committed implicit
  HE-AAC ADTS fixture (`tests/data/heaac_44100_48k_implicit_2s.aac`) with `AacDecoder::new().core_only()`
  and asserts a 22050 Hz output rate on every frame, 1024 samples per channel per frame,
  `sbr_implicit() == false`, `sbr_support() == NotSignalled` and `output_sample_rate() == Some(22050)`.
  Mutation check: removing the `disable_implicit_sbr()` call makes it fail (the rate assertion).
- No HE-AAC encoder is available here. `ffmpeg -c:a aac_he` is not supported, and `libfdk_aac`
  fails with `Unable to set the AOT 5`. The committed implicit HE-AAC fixture is therefore the
  generated HE-AAC-like case, and the regression test is on the exact faulty path.
- Test-harness and decoder behaviour are otherwise unchanged. No test was disabled or ignored.

## Verify
- Local, `CARGO_TARGET_DIR=~/.cache/tgt-aacsbrregress TMPDIR=~/.cache/tmp-aacsbrregress`:
  - `cargo test --release -p ec-aac --test sbr_real_library`: 16 passed, 0 failed (the failing test now passes).
  - `cargo test --release -p ec-aac` (full, local): lib 48 passed; implicit_sbr 4 passed;
    reelh_per_frame 1 passed; multichannel_parity 1 passed; no_panic 2 passed; obs_real_content
    1 passed; oracle 14 passed, 1 ignored; ps_mp4 1 passed; ps_oracle 1 passed; sbr_real_library 16 passed.
    Exit 0.
- server, `scripts/remote-run.sh aacsbrregress --host auto` (host c), non-personal targets:
  `cargo test --release -p ec-aac --lib --test implicit_sbr --test multichannel_parity --test no_panic --test ps_mp4 --test ps_oracle`:
  lib 48, implicit_sbr 4, multichannel_parity 1, no_panic 2, ps_mp4 1, ps_oracle 1. All passed, rc 0.
  `reelh_per_frame`, `obs_real_content`, `oracle` and `sbr_real_library` were not sent to the server:
  they read the user's personal media (`~/Downloads`, `~/Videos`, `~/Music`), and that data stays local.
- server, `cargo clippy -p ec-aac --all-targets -- -D warnings`: exit 0, before and after the
  formatting edit in the new test.
- `cargo fmt --check` already flags pre-existing hunks in this crate on main. None of the new
  lines are among them. The new test lines were formatted by hand.

## Left
- `core_only` does not undo the channel count forced to 2 by PS configs (`with_config` sets
  `channels = 2` for `ps_present`). The helper only calls it for SBR-only configs, so this is
  not exercised yet. It would matter for a PS-specific core-only caller.
- The `-576` lag in the test comments is stale. The full-chain low band now aligns at -961
  (corr 0.998) on the real-library mp4 and at 21566 (corr 0.997) on the second file. Both pass the
  0.99 gate, but the comment should be updated.
