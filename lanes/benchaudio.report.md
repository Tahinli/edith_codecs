# benchaudio: bench rows for the audio crates (DONE.md gate G6)

## Goal

Add local bench rows (DONE.md D7 exception: local 12-core machine, best of 5) for the crates that
had none: ec-aac decode (LC and HE-AAC v1) and encode (LC), ec-opus decode and encode, ec-vorbis
decode, ec-mp3 encode, ec-truehd decode, and ec-mp4 demux and mux. Each row records time, x realtime
(or packets/s and MB/s), and the output check against ffmpeg.

## Base

- Base sha: `edea19ce` (main, also the branch point of `lane/benchaudio`).
- Commit: see `git log -1 lane/benchaudio` (the lane commit adds only the files listed below; no
  crate source changed).

## Changes

- `crates/ec-aac/examples/bench.rs`: `dec` (ADTS to f32le, best of N) and `enc` (raw f32le to ADTS
  AAC-LC, best of N).
- `crates/ec-opus/examples/bench.rs`: `dec` (Ogg Opus, best of N) and `enc` (raw f32le to Ogg Opus,
  20 ms frames, `set_vbr_constrained(false)`, `set_bitrate`).
- `crates/ec-vorbis/examples/bench.rs`: `dec` (Ogg Vorbis, best of N).
- `crates/ec-mp3/examples/bench.rs`: `enc` (raw f32le to CBR Layer III, best of N).
- `crates/ec-truehd/examples/bench.rs`: `dec` (raw TrueHD, split with `frame_length`, best of N),
  prints the lossless self-check counters.
- `crates/ec-mp4/examples/bench.rs`: `demux` (all packets, packets/s and MB/s, plus per-track payload
  dumps) and `mux` (all packets into a new MP4, best of N).
- `scripts/bench-benchaudio.sh`: generates the clips (ffmpeg, synthetic), builds the six examples,
  waits for 1-minute load below 3 before each row, runs the rows, runs the ffmpeg checks, and prints
  `ROW|` and `CHECK|` lines. Options: `--no-clips`, `--no-build`.
- `lanes/bench.md`: new section `## lane benchaudio` with 10 rows and notes.

## Verify commands and results

Clippy, on server host c (`scripts/remote-run.sh benchaudio --host auto`):

```
cargo clippy -p ec-aac -p ec-opus -p ec-vorbis -p ec-mp3 -p ec-truehd -p ec-mp4 --all-targets -- -D warnings
```

Result: exit 0 (after one fix: `chunks_exact` replaced by `as_chunks::<4>()` in the three
examples that used it, which clippy 1.99 rejects under `-D warnings`).

Release tests, `cargo test --release`:

- `-p ec-mp4 -p ec-truehd -p ec-mp3 -p ec-vorbis` on host c: exit 0, all test binaries ok.
- `-p ec-aac -p ec-opus --no-fail-fast` on host a: exit 0, 16 of 16 test binaries ok.
- `-p ec-aac -p ec-opus` on host c (first attempt): `ec-opus` `conformance::decode_speed` FAILED,
  "opus-ogg-5.1-48000.opus: 94x realtime is below the 104x floor", then 101x on a rerun.
  The same test on host a: 125x, passes. The crate source is unchanged from `edea19ce`, so this is
  host speed, not this lane. The test is a wall-clock floor and is sensitive to the server's load.

File length: all new files are under 200 lines (gate: no source file over 5000 lines).

Bench run, local, `scripts/bench-benchaudio.sh` (final run, exit 0), numbers in `lanes/bench.md`
under `## lane benchaudio`. Summary:

| crate / op | content | best of 5 | x realtime / rate | check |
|---|---|---|---|---|
| ec-aac decode | AAC-LC 128k stereo 60 s | 0.0837 s | 717x | corr 1.000000 vs ffmpeg native aac |
| ec-aac decode | HE-AAC v1 stereo, 54.95 s (fixture x28) | 0.5528 s | 99.4x | corr 0.999998 vs ffmpeg native aac |
| ec-aac encode | AAC-LC 128k from 60 s PCM | 0.8022 s | 74.8x | ffmpeg decode corr 0.974786 vs source (FINDING) |
| ec-opus decode | 48k stereo 96k 60 s | 0.0878 s | 684x | corr 1.000000 vs ffmpeg, lag 312 (pre-skip) |
| ec-opus encode | 48k stereo 96k 60 s | 0.1770 s | 339x | ffmpeg decode corr 0.998553 vs source |
| ec-vorbis decode | q5 stereo 60 s | 0.0937 s | 640x | corr 1.000000 vs ffmpeg |
| ec-mp3 encode | 192k CBR stereo 60 s | 0.5145 s | 117x | ffmpeg decode corr 0.999972 vs source |
| ec-truehd decode | 5.1 24-bit 60 s | 0.3067 s | 196x | bit-exact, md5 6fb81f90 = ffmpeg |
| ec-mp4 demux | H.264 1080p30 + AAC, 4614 packets | 0.0018 s | 2.57 M packets/s, 17239 MB/s | payload md5 = ffmpeg, both tracks; 4614 = ffprobe |
| ec-mp4 mux | same packets to a new MP4 | 0.0143 s | 323k packets/s, 2171 MB/s | ffmpeg `-c copy -f data` md5 = source, both tracks |

Calibration rows (same check, ffmpeg's own AAC encoders at 128k): native aac corr 0.999845,
libfdk_aac corr 0.999937.

## Findings

1. ec-aac AAC-LC encoder quality: our 128k stereo output decodes in ffmpeg at correlation 0.9748
   with the source (SNR 12.5 dB, uniform over the file; least-squares output gain 0.913, corrected
   from 1.04 by lane aacencnoise). ffmpeg's encoders on the
   same source give 0.99985 and 0.99994. That is a real encoder gap (not a clip artifact), and it
   is not fixed here (bench lane, no source changes). It should get its own lane before any AAC
   encode quality claim. The check is reproduced by the script (the `CHECK|ec-aac encode` line).
2. HE-AAC encode is not available from the local ffmpeg: its libfdk_aac build fails every
   `aac_he` and `aac_he_v2` configuration tried (16 to 48 kHz, 24 to 96 kb/s, mono and stereo).
   The HE-AAC decode row therefore uses the repo's own HE-AAC v1 fixture
   (`crates/ec-aac/tests/data/heaac_44100_48k_implicit_2s.aac`, 2.14 s), repeated 28 times to about
   60 s. The fixture is project-generated, not personal media.
3. ec-opus decode leaves the 312-sample pre-skip in its output (ffmpeg trims it). Values match
   ffmpeg (correlation 1.000000 at lag 312). Whether to trim at the decoder is a product choice, not
   a speed row.
4. The ec-mp4 demux and mux numbers are page-cache bound (31 MB file held in RAM). They are upper
   bounds, not disk throughput.
5. `ec-opus conformance::decode_speed` 5.1 floor (104x) fails on host c (94x to 101x) and passes on
   host a (125x). Not caused by this lane. Worth a note in the server host notes.

## Clips and reproduction

All clips are synthetic, generated by `scripts/bench-benchaudio.sh` with the local ffmpeg 8.1.3, in
`~/.cache/benchaudio-clips` (outside the repo). The run needs no network.
Binaries go to `~/.cache/benchaudio-bin`, the build dir is `~/.cache/tgt-benchaudio`, the TMPDIR is
`~/.cache/tmp-benchaudio`. `/tmp` was not used.

Reproduce:

```
scripts/bench-benchaudio.sh
```

## Left open

- AAC-LC encoder quality gap (finding 1).
- The `decode_speed` floor on host c (finding 5) is a server matter.
- Not requested and not done: MP3 decode, Opus 5.1, and Vorbis encode rows.
