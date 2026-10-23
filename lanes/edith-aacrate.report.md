# edith lane aacrate: open implicit-SBR AAC at its decoded rate

Base: codecs-integration `1529ad4`. Branch `lane-aacrate`, commit `e769216`.
Depends on edith_codecs main `f708426b` (ec-aac implicit SBR, `lanes/aacimplicit.report.md`).

## Goal
A bare ADTS HE-AAC file (headers say AAC-LC at 22.05 kHz, frames carry SBR) opened at 22050 Hz
with the core's bandwidth. Track metadata should report the decoded rate (44100 for the fixture).

## What changed
- `crates/engine/src/audio.rs`
  - `AacPrimed` and `aac_prime`: a throwaway `ec_aac::AacDecoder` decodes up to `AAC_PRIME_UNITS`
    (2) access units. It stops early once `sbr_implicit()` is true. The rate is read back from
    `output_sample_rate()` and the channel count from the decoded block.
  - `aac_may_be_implicit`: true for no config (ADTS) or a plain-LC config (`SbrSupport::NotSignalled`).
    Explicit SBR configs state their rate and are not decoded at open, so that path is unchanged.
  - `sym_aac_prime` (ADTS and Matroska, via a fresh `sym_reader`) and `mp4_aac_prime` (mp4 `AacTrack`,
    via a fresh `Mp4Reader`, since the track borrows its reader). Both are used only when the
    rate may be implicit.
  - mp4 `AacTrack`: an implicit payload sets `sbr`, so packet copy refuses the track (the copy would
    write an LC config at the core rate).
  - Decode errors during priming fall back to the container rate. Playback reports them as before.
- `crates/engine/tests/audio_aac.rs`: three new tests.
- `crates/engine/tests/data/heaac_44100_48k_implicit_2s.aac` (new, 13.6 KB): copied from
  edith_codecs `crates/ec-aac/tests/data/`.

## Tests
- `bare_adts_he_aac_opens_at_its_implicit_sbr_rate`: opens at (44100, 2); 94208 frames per channel
  (2.136 s, the length ffmpeg decodes too, so the bound is 1.9 to 2.2 s); both channels non-silent.
- `bare_adts_he_aac_matches_ffmpeg_per_channel`: correlation >= 0.999 per channel with ffmpeg.
- `mp4_he_aac_with_an_implicit_config_opens_at_its_sbr_rate`: ffmpeg `-c copy` remux to m4a gives a
  plain-LC config at 22.05 kHz, and the track opens at 44100 Hz (this exercises `mp4_aac_prime`).

## Verify (server, `scripts/remote-run.sh edith-aacrate --with-edith`)
- `cargo test -p engine --release --test audio_aac`: 7/7 pass (4 existing, 3 new).
- `cargo test -p engine --release --lib tonemap::tests::perf_4k_and_1080p`: pass on rerun. It failed
  once in the full run (31.3 ms/frame over a 25 ms limit under load). Timing only, not AAC.
- Full `cargo test -p engine --release --no-fail-fast`, same name on both trees:
  - mine: 440 passed, 399 failed (before the clippy-only rewrite to let chains and as_chunks; no behaviour change).
  - codecs-integration baseline: 438 passed, 399 failed.
  - Failures are missing `assets/` media (for example `saveload` "copy the fixture: NotFound",
    `waveform` "5.1 AAC track ... NotFound") plus timing-sensitive tests. The sets differ by a few
    names (for example `a_cancelled_audio_export_leaves_no_file` fails only on the baseline, and
    `a_caption_window_no_project_can_hold_is_refused_by_name` fails only on mine). Both are
    missing-media or load-related, not AAC code. Not every name was checked individually.
- `cargo clippy -p engine --all-targets` (no `-D warnings`, the lane convention for engine):
  no new findings. The new `collapsible_if` in my code is fixed with a let chain, and the new
  `chunks_exact` in my test uses `as_chunks`. Remaining audio.rs and audio_aac.rs findings are
  pre-existing (too_many_arguments at 768/792/815, chunks_exact at 2493 and in the existing LC test).
  Note: the full clippy listings are not directly comparable because the baseline log lacks the
  backtick quoting of the newer rustc output. The per-site comparison was done by hand.

## Not done / caveats
- Only the first two access units are probed. A stream whose first SBR payload comes later than frame 2
  would open at the core rate. Every SBR frame carries a payload (per the aacimplicit report), so this
  does not occur in the fixtures.
- A mid-stream segment decoder (fresh `AacDecoder` per segment) switches on its first SBR frame, as
  noted in the aacimplicit report. Not changed here.
- Downsampled SBR (core above 24 kHz) is still not reconstructed in ec-aac.
- The ADTS timebase is left at the header rate: 1024 core samples per frame at 22.05 kHz equals 2048
  output samples at 44.1 kHz, so the timeline is consistent.
