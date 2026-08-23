# lane-vorbis-psy — round 3 handoff (2026-08-23)

Branch `lane-vorbis-psy` @ 689cdc9 + this commit. No merge, no push.

## What this round did

- Added `band_error_vs_reference` (ignored instrument test) to
  `crates/ec-vorbis/tests/oracle.rs` — 2048-pt Hann STFT, 24 bands from 25 edges
  (100 Hz–24 kHz), band NSR + band-energy-vs-source for ours vs ffmpeg libvorbis,
  both decoded through our decoder. Bit-split oracle folded into the same pass
  (see deviations).
- Ran all 14 rows (7 sources × {96,128} kbps, 600 s each), release mode, 163 s.
- `cargo test -p ec-vorbis` non-ignored: green (13 lib + 8 oracle, 3 ignored).
- Encoder output byte-identical to main — established at 689cdc9 in round 2
  (sadie 120 s 128k, 4b007cd vs 689cdc9), not re-proven this round; this round
  touches only `tests/oracle.rs` + lanes files.

## Findings (numbers in `vorbis-psy-r1.report.md`)

1. **Residue coding efficiency is the dominant gap.** bits/non-zero = 1.48–2.12×
   ref on all 14 rows; we emit 45–57% as many non-zeros at 4.8–6.6 bits each vs
   ref's 2.4–3.8, from near-identical residue budgets (±4%).
2. **The error is concentrated at 300–1080 Hz** — dNSR +8.6 to +14.6 dB; ref hits
   NSR −20 to −30 there, we sit at −8 to −20. Mid 1–5 kHz +2 to +8. Above 7.7 kHz
   we win or tie on most rows.
3. **HF droop is source-dependent, not universal**: nik 15500-24000 E −19 dB
   (hard lowpass, both rates; ref −1.8/−2.3), sadie −7.0/−5.3, hein −6.8/−5.3,
   naz@96 −3.1. zaur/her/dl8a none.
4. Floor spend: ours 0.74–0.99× ref; ref outspends us most on sadie/hein 128k
   (15.5–15.7 k b/s vs 11.6k) — same sources where HF droop shows.

## Next-round candidates (ranked by measured impact)

- (1)+(2) Residue: encode ~2× non-zeros at ~half bits each in low bands —
  suspect book selection / dimension / amplitude-bit allocation, not psy
  (budget already matches).
- (3) nik 15.5 kHz cutoff: look for a frequency-limit or floor1 cutoff in the
  psy/floor path that engages on that source; sadie/hein droop likely floor
  underspend (finding 4).
- (4) Tonal 1–5 kHz on her/zaur/dl8a — minor (+3.1 to +6.5), fix after (1).

## Deviations from this round's charter

- Bit-split oracle measured inside `band_error_vs_reference` rather than the
  12 s × 128k-only `residue_histogram_vs_reference`: same
  `decode_capture_with_bits` + `bit_split_summary` on the same files, one encode
  pass instead of two, extended to all 14 rows.
- "25 Bark bands" in the charter vs 25 edges listed → 24 bands; edges taken as
  authoritative.

## Files

- `crates/ec-vorbis/tests/oracle.rs` — new test (+ fft helper), 244 lines.
- `lanes/vorbis-psy-r1.bands.txt` — 394 lines, per-row band tables.
- `lanes/vorbis-psy-r1.bits.txt` — 71 lines, per-row bit split.
- `lanes/vorbis-psy-r1.report.md` — verdict, tables, diagnosis.
- Scratch oggs under `$CARGO_TARGET_DIR/.../scratch/vorbis7-bands/` (not
  committed).

Worktree intentionally live for round 4.
