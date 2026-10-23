# lane vorbistrim: Ogg Vorbis final-granule trim in ec-vorbis

Base: main 8fb9fb6c.

## Goal

A consumer that decodes a whole Ogg Vorbis file must get exactly the sample count
the stream states, not the block grid's overshoot. Before this change, the bare
`ec_vorbis::VorbisDecoder` (the `Decoder` trait path) emitted every decoded block
plus the flush tail, and never looked at the page granules.

## Measured before the change

Six ffmpeg/libvorbis files (sine, `-q:a 4`), decoded through `OggDemuxer` +
`VorbisDecoder` `send_packet`/`receive_frame`/`flush`. Overshoot against the
granule-stated length (= ffmpeg's count on these files, except as noted below):

| file | granule (= length) | old trait output | overshoot |
|---|---|---|---|
| 44.1k mono 1.000 s | 44100 | 45632 | +1532 |
| 44.1k stereo 1.234 s | 54419 | 55867 | +1453 |
| 48k mono 2.5 s | 120000 | 121408 | +1408 |
| 48k stereo 1.000 s | 48000 | 49728 | +1728 |
| 44.1k stereo 2.5 s | 110250 | 112192 | +1942 |
| 48k mono 1.234 s | 59232 | 60992 | +1760 |

`ec-probe`'s `AudioDecoder` (the probe path) already trimmed these at its own level,
so its output matched the granule; but it did so with a Vorbis-specific tail
rule that a direct `VorbisDecoder` consumer did not get.

## Changes

- `crates/ec-vorbis/src/trim.rs` (new): `GranuleTrim`. Decoded samples are held
  until the granules say which are the stream's. Releases only up to the latest
  granule; the flush cuts the rest. The offset `granule - decoded` is taken from
  the first granule that arrives before any output (a head trim, or a non-zero
  start); otherwise the offset is zero. Later granules only move the end.
- `crates/ec-vorbis/src/decode.rs`: `send_packet` and `flush` go through
  `GranuleTrim` (the granule is read from the packet's `ec-ogg` side data, kind
  `OggS`, the same constant `ec_ogg::GRANULE_KIND` defines). `reset_state` clears
  it. `decode_audio` is unchanged and still returns raw, untrimmed block output.
- `crates/ec-probe/src/decoder.rs`: the Vorbis path no longer runs its own
  front/tail trim, because `ec-vorbis` now does it; a second pass would drop real
  samples. The dead `held` buffer (only Vorbis filled it) is removed. Other
  codecs are unchanged.
- `crates/ec-vorbis/tests/trim.rs` (new): three tests (below).

## API edith (and any consumer) should call

Whole-file decode with trimming:

1. `OggDemuxer::open(file)`; take the Vorbis stream; `VorbisDecoder::from_extradata(&stream.params.extradata)`.
2. For each packet of that stream from `next_packet()`: `send_packet(&packet)`, then drain `receive_frame()` until NeedMore.
3. `flush()`, then drain `receive_frame()` until Eof.

The packets from `OggDemuxer` carry the page granule as side data, so step 2 needs
nothing extra. Concatenated frames are the stream's length.

Via `ec_probe::Reader`: `make_decoder(stream)`, `next_packet`, `decode`, then `flush`
(public API unchanged; the trim now lives in `ec-vorbis`).

Do not use `VorbisDecoder::decode_audio` for whole files: it is the raw block output
and does not trim.

## Verify commands and results

Run on server host b (a and c were busy at the time), with
`CARGO_TARGET_DIR=$HOME/.cache/tgt-vorbistrim TMPDIR=$HOME/tmp/vorbistrim`:

- `scripts/remote-run.sh vorbistrim --host auto -- cargo test --release -p ec-vorbis`
  - lib unit tests: 14 passed
  - `tests/oracle.rs`: 8 passed, 3 ignored (the same 3 `#[ignore]`s as main)
  - `tests/trim.rs`: 3 passed
- `cargo clippy -p ec-vorbis --all-targets -- -D warnings`: clean (exit 0, zero diagnostics)
- `cargo test --release -p ec-probe` (ec-probe changed): 7 + 7 + 6 passed
- `cargo clippy -p ec-probe --all-targets -- -D warnings`: does NOT build. It fails in
  `ec-alac` (a dependency this lane did not touch) on `chunks_exact` with a constant
  chunk size. Not fixed here.
- `ec-ogg` was not touched and was not tested.

The new tests in `tests/trim.rs`:

1. `whole_file_length_and_samples_match_ffmpeg`: six multi-page files (44.1k and
   48k; mono and stereo; 1.234, 2.5, 3.0 s). Asserts exact length equal to
   ffmpeg's, and per-channel correlation >= 0.999 against ffmpeg. All pass.
2. `single_page_file_ends_at_its_granule`: 44.1k mono and 48k stereo at 1.0 s.
   Asserts length equals the granule (44100 / 48000) and correlation >= 0.999
   against ffmpeg's samples. See the deviation below.
3. `granule_behind_the_first_output_trims_the_head`: moves the granule of the first
   output-producing packet to `decoded - 300`, so the first 300 samples are a
   head trim. Asserts the output is exactly the untrimmed decode from sample 300
   (bit-exact). Passes.

## What is left / deviations

- **Single-page files: ffmpeg is 128 samples short.** When the whole audio fits
  one Ogg page, the only granule is the final one. ffmpeg then decodes 43972 /
  47872 frames where the granule states 44100 / 48000. ffprobe shows the
  first packet at pts 0 for these files, and at pts -128 on multi-page files.
  The decoder follows the granule here. Test 2 documents the difference and
  checks our samples agree with ffmpeg's where both have them.
- **Chained streams** are not supported: `ec-ogg` reads the first chain only
  (documented in `ec-ogg`'s crate docs). A non-zero start is covered by test 3
  (a synthetic granule), not by a real file.
- The `ec-alac` clippy failure blocks `cargo clippy -p ec-probe`. Outside this lane.
