# Lane truehdexact: ec-truehd bit-exact, ec-ac3 real-library check

Branch `lane/truehdexact`, base `bdc74c55` (main). History rewritten (unpushed) so no copy of the remux clip is in any commit. DONE.md items: ec-truehd decode (P1), ec-ac3 decode (P0).

## Goal

- ec-truehd: the 2-, 6- and 8-channel substreams bit-exact vs ffmpeg, not only correlation > 0.99.
- ec-ac3: AC-3 and E-AC-3 through 5.1 match ffmpeg on the real_library.toml entries, and the
  E-AC-3 7.1 dependent-substream case (D5) holds.

## Result

- ec-truehd: all five fixtures decode bit-exact (sample for sample) against ffmpeg. No decoder
  change was needed. The existing `oracle.rs` threshold (>= 0.9999) is unchanged.
- ec-ac3: the two real_library entries with AC-3 or E-AC-3 are at correlation >= 0.999 per
  channel. Outputs are not bit-exact, and the cause is probably dither (see below).

## Changes

- `crates/ec-truehd/fixtures/` (new, 4 files, 426 KB total, each under 200 KB):
  `truehd-s16-2ch.mkv`, `truehd-s24-2ch.mkv`, `truehd-s16-51.mkv`, `truehd-s24-51.mkv`,
  made with ffmpeg 8.1.3 `-c:a truehd -strict -2` on server host c, 48 kHz, from `aevalsrc`
  tones (s16p and s32p input; the s32 case adds a 0.001 random floor).
  No copyrighted audio is committed.
- `crates/ec-truehd/tests/fixture_exact.rs` (new):
  - `fixtures_are_bit_exact_against_ffmpeg`: demuxes each committed fixture with ec-matroska,
    decodes with `TrueHdDecoder`, decodes with ffmpeg (`-f s32le`), and asserts equal sample
    counts, equal channels and rate 48 kHz, and zero first mismatch. It also asserts the
    decoder's self-check counters are zero.
  - `remux_71_is_bit_exact_against_ffmpeg`: the 7.1 case, from a local Blu-ray remux. The path
    comes from env `EC_TRUEHD_71_REMUX`, or the `truehd_71_remux` key of `real_library.toml`
    (env `EC_REAL_LIBRARY` overrides the file). It decodes the first 2 s from the start of the
    file and compares all 768,000 samples with ffmpeg `-t 2`. It skips with a message when the
    path is unset or absent.
- `crates/ec-ac3/tests/real_library.rs` (new): reads `real_library.toml` (env
  `EC_REAL_LIBRARY`, default `~/Documents/Code/Rust/edith/real_library.toml`). For each
  entry whose first audio track is AC-3 or E-AC-3, it copies 20 s (`EC_REAL_SECONDS`) with
  `-c copy`, decodes with ec-ac3 (dither off, as in `decode_matrix.rs`) and with ffmpeg, and
  gates per channel: correlation >= 0.999. A channel whose reference RMS is below 1e-4
  (silent LFE) is gated on RMS <= 1e-6 instead, because correlation on rounding noise is
  meaningless. Missing files and non-AC-3 tracks skip loudly.
- `DONE.md`: two DONE log lines (ec-truehd, ec-ac3).

## Fixture decision

- ffmpeg's TrueHD encoder supports only mono, stereo, 2.1, 3.0, 3.1, 4.0, 4.1, 5.0(side) and
  5.1(side). It refuses 7.1 ("Specified channel layout '7.1' is not supported by the truehd
  encoder"). The 7.1 case therefore uses a local real remux, read by path and not committed.
- 16-bit fixtures are s16p input, 24-bit are s32p input. ffprobe reports 24 bits for both.
  That is the encoder's output format, and it was not checked against the stream header.

## ec-truehd per-file results

Command: `cargo test --release -p ec-truehd --test fixture_exact -- --nocapture`, with
`EC_TRUEHD_71_REMUX` set to the local remux for the 7.1 row.

| File | Layout | Input bits | Size | Frames | Samples | vs ffmpeg |
| --- | --- | --- | --- | --- | --- | --- |
| truehd-s16-2ch.mkv | 2.0 | 16 | 66,951 B | 48,000 | 96,000 | bit-exact |
| truehd-s24-2ch.mkv | 2.0 | 24 | 103,022 B | 24,000 | 48,000 | bit-exact |
| truehd-s16-51.mkv | 5.1 | 16 | 151,141 B | 48,000 | 288,000 | bit-exact |
| truehd-s24-51.mkv | 5.1 | 24 | 105,658 B | 12,000 | 72,000 | bit-exact |
| local remux (not committed) | 7.1 | 24 (real remux) | n/a | 2 s | 768,000 | bit-exact |

Frames are per-channel sample frames decoded. Tracks are 48 kHz.

## ec-ac3 real_library per-file results

Command: `cargo test --release -p ec-ac3 --test real_library -- --nocapture`, first 20 s.

| Entry | File codec, layout | Channel | Correlation | RMS delta | Exact samples | Gate |
| --- | --- | --- | --- | --- | --- | --- |
| hevc_4k_hdr | E-AC-3, 5.1(side) | FL | 0.999999 | 3.49e-6 | 20 / 960,000 | corr |
| | | FR | 0.999998 | 3.91e-6 | 16 / 960,000 | corr |
| | | FC | 0.999999 | 7.55e-6 | 89 / 960,000 | corr |
| | | LFE | 0.777 (silent) | 6.34e-8 | 758,668 / 960,000 | RMS |
| | | BL | 1.000000 | 1.84e-6 | 14 / 960,000 | corr |
| | | BR | 1.000000 | 1.78e-6 | 22 / 960,000 | corr |
| ac3_51_film | AC-3, 5.1(side) | FL | 0.999796 | 3.35e-4 | 12 / 960,000 | corr |
| | | FR | 0.999778 | 3.52e-4 | 10 / 960,000 | corr |
| | | FC | 0.999994 | 7.38e-5 | 16 / 960,000 | corr |
| | | LFE | 1.000000 (silent) | 6.50e-8 | 488,142 / 960,000 | RMS |
| | | BL | 0.999986 | 5.33e-5 | 11 / 960,000 | corr |
| | | BR | 0.999995 | 3.60e-5 | 13 / 960,000 | corr |

Both files pass. Other real_library entries have no AC-3 or E-AC-3 first track: h264_dual_audio
(AAC), hevc_4k_pgs (AAC), av1_opus_71 and av1_opus_51 (Opus). Those four were skipped.

The LFE correlation of 0.777 in the first hevc_4k_hdr run is what the silent-channel gate
fixes. The RMS delta on that channel is 6e-8, so the gate was added rather than the threshold
lowered.

Not bit-exact, and why: the two decoders' output differs by 1e-6 to 3e-4 RMS. Our dither is
off. ffmpeg's AC-3 decoder is believed to dither zero-bit mantissas with its own sequence, and
the spec leaves that sequence free (see `Options::dither` in `crates/ec-ac3/src/decoder.rs`).
This was not verified by a separate run. The ac3_51_film gap (3e-4 RMS) is larger than the
E-AC-3 gap (4e-6), so the cause may not be dither alone.

## ec-ac3 matrix tests (fixtures/audio, incl. D5 7.1)

| Binary | Result |
| --- | --- |
| lib | 48 passed |
| decode_matrix (incl. fixture_matrix_matches_ffmpeg) | 8 passed |
| dependent (7.1 E-AC-3, D5) | 7 passed |
| encode_matrix | 6 passed |
| real_library | 1 passed |

## Verify

Run locally (see Compute below):

- `cargo test --release -p ec-truehd --no-fail-fast`: lib 17, atmos_synth 2, fixture_exact 2
  (incl. the 7.1 remux), library 1, oracle 3. All passed, 0 failed.
- The 7.1 test without the env var (and no toml key): skips with its message, 2 passed.
- `cargo test --release -p ec-ac3 --no-fail-fast`: all binaries above passed, 0 failed.
- `cargo clippy -j4 -p ec-truehd -p ec-ac3 --all-targets -- -D warnings`: exit 0, no warnings.

One flake was seen. `stack_budget::tests::test_threads_get_the_stack_they_are_given`
(ec-ac3 lib) failed in 2 of 8 runs on this host, with "the mapping holding an
explicitly-1-MiB thread's frame is 807460864 bytes". It passed in the other 6 runs, including
3 more in isolation. The test reads `/proc/self/maps` and assumes a stack VMA under 512 MiB.
Its failure is environmental and not caused by this lane (no ec-ac3 src change).

## Compute

Every `remote-run.sh truehdexact` attempt waited on busy slots on hosts a, b and c for the whole
period. Other lanes held the slots. The runs were made locally with
`CARGO_TARGET_DIR=~/.cache/tgt-truehdexact TMPDIR=~/.cache/tmp-truehdexact -j4`, which is the
fallback the lane rules allow. Fixture generation and the 7.1 TrueHD source were on server host c.

## Caveats

- The 7.1 case is not committed. It reads a local Blu-ray remux by path, so the repository
  holds no copyrighted audio. The test runs only where that file exists. ffmpeg cannot encode
  7.1, so no generated 8-channel fixture is available.
- No 2-, 6- or 8-channel TrueHD file came from real_library.toml. The real TrueHD remux on
  disk is the only real source.
- `ac3dep` (D5) was tested with the matrix fixtures, not a real Dolby 7.1 stream. The ffmpeg
  defect in `lanes/ac3dep.report.md` (SL/SR placement) still applies.

## Left

1. Check whether the AC-3 difference on ac3_51_film is only dither, by turning dither on in
   both decoders with the same sequence, or by a dither-off run in an ffmpeg build that
   disables it.
2. Make `stack_budget`'s upper bound tolerant of merged VMAs, or have it read `/proc/self/maps`
   with a stricter match.
