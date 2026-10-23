# lane tstruehd: TrueHD (stream type 0x83) demux in ec-mpegts

Base: `main` (`5e4ef957`), branch `lane/tstruehd`. Not pushed.

## Goal

ec-mpegts refused stream type 0x83 (Dolby TrueHD in Blu-ray M2TS/TS), so edith could not
decode TrueHD inside transport streams. Expose it as `CodecId::TrueHd`, one packet per
access unit that `ec-truehd` decodes, and handle the AC-3 core that Blu-ray interleaves in
the same PID.

## Changes

- `crates/ec-mpegts/src/codec.rs`: new `StreamKind::TrueHd` for 0x83 (was
  `Other("truehd")`, a refusal), named `truehd`, mapped to `CodecId::TrueHd`.
- `crates/ec-mpegts/src/track.rs`:
  - `truehd_push`: packetiser for the stream. At each position it first checks for an AC-3
    syncframe (0x0B77, `ac3_header`) and skips the whole frame. Otherwise it reads the
    access-unit length word (`ec_truehd::frame_length`) and requires the access unit to
    parse with `ec_truehd::AccessUnitHeader::parse`, so its substream directory must sum to
    its length. Positions that fail both are skipped one byte at a time, as the other audio
    packetisers do. Incomplete units wait for the next PES (carry, as in `audio_push`).
  - Each packet is one whole access unit starting at its 4-byte header (length word and
    input timing), the same form the MP4 (`mlpa`) and Matroska (`A_TRUEHD`) demuxers hand to
    `TrueHdDecoder`.
  - `TrueHdFacts` (rate, channel layout, samples per access unit) is taken from the major
    sync. Samples per access unit are `40 << (rate_code & 7)`.
  - `ready()` and `stream_info()` expose the stream once a major sync has been seen. It is
    `CodecId::TrueHd`, `AudioParameters` with `format: None`, and decodes to S32.
- `crates/ec-mpegts/Cargo.toml`: depends on `ec-truehd` (no cycle; ec-truehd depends only on
  ec-core). `Cargo.lock` updated accordingly.
- `crates/ec-mpegts/src/lib.rs`: module docs describe TrueHD and the AC-3 handling.
- `crates/ec-mpegts/tests/synthetic.rs`: the refusal test for 0x83 now asserts TrueHD is
  exposed (kind `TrueHd`, no refusal, stream not yet seen since no access unit was sent).

## AC-3 core handling

AC-3 core frames interleaved in the TrueHD PID are dropped, not exposed. The TrueHD access
units carry the same audio losslessly, so the AC-3 frames take no time on the timeline.
Timestamps come from the PES PTS of the first access unit that starts in a PES and advance
by the access unit's sample count, so the AC-3 frames do not shift them.

## Tests

New `crates/ec-mpegts/tests/truehd.rs`, fixtures generated at test time (no media committed):

- `truehd_in_ts_decodes_sample_exact_to_ffmpeg`: ffmpeg lavfi sine, 2 ch, 48 kHz,
  `-c:a truehd -strict -2 -f mpegts` (ffmpeg writes stream type 0x83). Every packet is a
  whole AU (length word matches). Decoded through `TrueHdDecoder`; check stats are zero; the
  S32 PCM equals ffmpeg's `s32le` decode sample for sample.
- `truehd_packets_match_the_container_packet_form`: the access-unit bytes equal ffmpeg's
  raw `-f truehd` stream copy.
- `ac3_core_interleaved_in_the_truehd_pid_is_dropped`: a hand-built TS (small PAT/PMT/PES
  writer in the test) with a raw ffmpeg TrueHD stream split into AUs, with an AC-3 frame
  before each AU in the same PID. Demuxed packets are exactly the AUs (no AC-3 bytes), with
  PTS `90000 + k*75` (40 samples at 48 kHz).

Synthetic refusal test updated as described above.

## Verify

Gate on the server (host c, auto):

    scripts/remote-run.sh tstruehd --host auto -- bash -c 'cargo clippy -q -p ec-mpegts --all-targets -- -D warnings; echo CLIPPY=$?; EC_REQUIRE_FIXTURES=1 cargo test --release -p ec-mpegts -p ec-truehd --no-fail-fast 2>&1 | grep -E "^test result|FAILED|panicked"'

- `CLIPPY=0`.
- Passed: 105 tests across the ec-mpegts and ec-truehd test binaries (includes the 3 new
  truehd tests).
- Failed: 2 tests in `crates/ec-truehd/tests/atmos_synth.rs`
  (`object_substream_contents_do_not_change_the_pcm_output`,
  `synthetic_atmos_decodes_bit_exact_to_the_original_7_1`). They need a personal Blu-ray
  remux under `~/Downloads` (the "ReelR ... TrueHD" file), which the server does not
  have, so under `EC_REQUIRE_FIXTURES=1` they fail by design. ec-truehd is not changed by this
  lane.
