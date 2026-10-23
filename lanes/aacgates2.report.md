# Lane aacgates2: ec-aac gates (file size, encoder score and rate, PS, mp4 refusal)

## Goal
Close the open ec-aac items in DONE.md: (1) no ec-aac file over 5k lines; (2) an objective
encoder score against ffmpeg's native AAC encoder at 128 kbps stereo; (3) CBR rate within
+-5% at 64, 128, 256 kbps stereo and at 5.1; (4) confirm the PS tone fixtures; (5) remove any
HE-AAC v2 / PS refusal in the ec-aac or ec-mp4 path.

Base sha: `bdc74c55` (branch `lane/aacgates2`).

## Changes
1. **File size (gate 7).** Mechanical splits, no code edits beyond visibility and the
   doc-comment boundary:
   - `crates/ec-aac/src/sbr_bands.rs` (6,343 lines) became `src/sbr_bands/mod.rs` (251, the
     non-test code), `src/sbr_bands/tests.rs` (86, the test functions) and
     `src/sbr_bands/vectors/{mod.rs,part0.rs,part1.rs}` (the 500 literal reference vectors,
     split in two; `VECTORS` is now `&[&[Vector]]`, and the test iterates the flattened parts).
   - `crates/ec-aac/tests/sbr_real_library.rs` (5,063 lines) became
     `tests/sbr_real_library/{main.rs,helpers.rs,chunk_a.rs,chunk_b.rs,chunk_c.rs}`. The
     cargo target name is unchanged. Helpers and chunks use `pub(super)` visibility, and
     `Candidate`'s fields are `pub(super)`.
   - Largest ec-aac file is now 4,305 lines (`sbr_bands/vectors/part0.rs`). Checked with
     `find crates/ec-aac -name '*.rs' | xargs wc -l`.
2. **Encoder score (item 2).** `scripts/aac-encode-score.sh` and `scripts/aac_score.py`:
   ffmpeg 8.1.3 native `aac` encoder and `ec-aac` (new example `crates/ec-aac/examples/aacenc_file.rs`)
   encode the same 10 s, 48 kHz clip. Both are decoded with ffmpeg, aligned by a cross-correlation
   lag search (0..4096 samples), and scored.
   - Clips: four stereo music tracks from the user's `~/Music` (30 s into each track). The
     fixtures in `fixtures/audio` contain no music longer than 3 s, so they were not usable
     as music clips. The 5.1 clips are a synthetic upmix of the stereo clip (pan filter).
   - Metric choice: ViSQOL is not on pip, and there is no PEAQ/PESQ tool that fits music
     without a build step. No sudo is available for installing one. The score is therefore
     SNR, segmental SNR (2048-sample frames, clipped to [-10, 35] dB) and a band NSR proxy for
     NMR (20 log bands, 10 log10(noise/signal), clipped to [-40, 10] dB). The NSR proxy has no
     psychoacoustic masking, so it is not a standard NMR.
3. **Rate (item 3).** Measured from the encoded ADTS size over the reference duration.
4. **PS (item 4).** Not re-investigated in this lane. The sine-tone mismatch was fixed by lane
   `aacpstone` (merge `57684c50`, included in this base as `be4024eb`): `ps_data::build_codes`
   assigns Huffman codes in table order as FFmpeg does, the sine fixtures are at corr 1.000000,
   and `KNOWN_INEXACT` in `tests/ps_oracle.rs` is empty. The stale `lanes/aacps.report.md`
   still lists the tones as inexact.
5. **mp4 PS path (item 5).** No refusal of HE-AAC v2 or PS by name exists in `ec-aac` or
   `ec-mp4`. The `ec-mp4` `asc::aac_output` already maps object type 29 to two channels, and the
   `ec-aac` decoder handles PS through `with_config_bytes`. The only `unsupported` sites in the
   two crates are for predictor/main profile, SSR, coupling, mp4 sample entries and stz2 field
   sizes. I added `crates/ec-aac/tests/ps_mp4.rs`: the PS fixture is remuxed to m4a by ffmpeg
   (stream copy), read back through `Mp4Demuxer`, and decoded through the ASC.
   - The ffmpeg remux gives an implicit HE-AACv2 ASC (LC at 24 kHz). Before the first frame,
     `output_sample_rate()` reports 24000, which is the core rate. The decoder switches to 48 kHz
     stereo once the first frame reveals the extension. The test therefore asserts the rate
     on the decoded frames, not before decoding.
   - Result: `ps mp4: ours 208896 samples, ffmpeg 208896 samples, corr 0.999977`.

## Encoder results (vs ffmpeg native AAC, 10 s clips, zero-lag-corrected lag 1024 for both)

Score values: SNR dB, segSNR dB, band NSR dB (lower is better), and encoded kbps.

| clip | config | encoder | SNR | segSNR | NSR | kbps |
|---|---|---|---|---|---|---|
| clip1 | stereo 128 | ffmpeg | 16.66 | 18.62 | -12.88 | 131.55 |
| clip1 | stereo 128 | ec-aac | 7.01 | 7.84 | -6.00 | 130.90 |
| clip2 | stereo 128 | ffmpeg | 32.14 | 32.44 | -21.05 | 130.67 |
| clip2 | stereo 128 | ec-aac | 20.42 | 21.86 | -16.16 | 130.94 |
| clip3 | stereo 128 | ffmpeg | 21.58 | 23.36 | -18.28 | 130.92 |
| clip3 | stereo 128 | ec-aac | 12.47 | 12.21 | -11.33 | 130.92 |
| clip4 | stereo 128 | ffmpeg | 24.47 | 25.96 | -17.35 | 132.05 |
| clip4 | stereo 128 | ec-aac | 15.50 | 15.96 | -11.66 | 130.95 |

The other configs are in `scorelab/` (not committed) and show the same ordering: ec-aac is
behind ffmpeg on SNR, segSNR and NSR at 64, 256 kbps and 5.1 384 kbps too. Mean SNR gap at
128 kbps stereo is about 10 dB. The gap is a waveform-metric gap, and it is not a
perceptual result.

**Objective score: ec-aac is about 9 to 12 dB behind ffmpeg's native encoder on SNR and
segSNR at 128 kbps stereo.** The 0.999 correlation target and the INCUMBENT_BAR in
`tests/oracle.rs` (worst channel 0.9743 at 128) are a different measure, and the bar is still
met.

## Rate results (CBR, actual kbps from ADTS size)

| config | target | clip1 | clip2 | clip3 | clip4 | max deviation |
|---|---|---|---|---|---|---|
| stereo 64 | 64 | 66.79 | 66.74 | 66.78 | 66.78 | +4.4% |
| stereo 128 | 128 | 130.90 | 130.94 | 130.92 | 130.95 | +2.3% |
| stereo 256 | 256 | 259.26 | 259.28 | 259.31 | 259.30 | +1.3% |
| 5.1 384 | 384 | 387.52 | 387.49 | 387.65 | 387.59 | +0.9% |

All within +-5%. The 64 kbps case is the closest to the limit (+4.4%) and should be watched.

## Verify
Each command ran through `scripts/remote-run.sh aacgates2 --host auto`.
- `cargo clippy -p ec-aac --all-targets -- -D warnings`: clean (run on host a, after fixing
  one `chunks_exact` lint in the new `ps_mp4.rs` and `aacenc_file.rs`).
- `cargo test --release -p ec-aac --no-fail-fast`: 86 passed, 0 failed, 1 ignored
  (the pre-existing ignored test). Run on host a, final log `scratchpad/final3.log`.
- `cargo check -p ec-aac --all-targets`: clean after the split.
- No ec-aac file over 5,000 lines (largest 4,305).

## What is left
- Item 2's score is recorded, but ec-aac is about 10 dB behind on SNR at 128 kbps. Closing
  that is a separate encoder-quality task, and the score does not claim parity.
- The 64 kbps rate is +4.4%, close to the +5% limit. A tighter rate control would help.
- Item 4 has no new evidence in this lane. The PS fixtures are covered by `aacpstone`.
- `scripts/remote-run.sh` in this worktree has a local uncommitted change from main (three
  slots); it is not part of this lane and is not committed here.
- `output_sample_rate()` reports the core rate for an implicit HE-AACv2 config until the first
  frame. The edith side should not read the rate before decoding.
