byte-identity: IDENTICAL (sadie 120 s 128k, main 4b007cd vs 689cdc9)

# vorbis-psy r3 — band error + bit split, 7 sources × {96k, 128k}

Instrument: `band_error_vs_reference` (`crates/ec-vorbis/tests/oracle.rs`), run in
release, full 14 rows (no `SWEEP_ONLY` fallback needed; 163 s total, 3–25 s/row).
Ref = ffmpeg libvorbis `-t 600 -ac 2 -ar 48000`; ours encoded at the ref's measured
kbps; both decoded through **our** decoder, so the gap measures encoders only.
Bands: 2048-pt Hann, hop 1024, 25 edges 100 Hz–24 kHz → 24 bands, both channels
summed. NSR/E in dB vs the source per band; dNSR = NSR_ours − NSR_ref (positive =
we're worse). Raw tables: `vorbis-psy-r1.bands.txt`, `vorbis-psy-r1.bits.txt`.

## Bit split (floor / residue / bits-per-nonzero, ours vs ref, ratio)

| row | floor ours/ref b/s | residue ours/ref b/s | nz ours/ref | bits/nz ours vs ref | ratio bits/nz |
|---|---|---|---|---|---|
| nik 96k | 9723 / 10286 | 72917 / 73048 | 2.56M / 4.76M | 5.42 vs 2.92 | **1.86** |
| nik 128k | 9645 / 10077 | 103989 / 104124 | 3.81M / 5.63M | 5.19 vs 3.52 | **1.48** |
| zaur 96k | 9807 / 9941 | 70346 / 70613 | 2.99M / 5.78M | 5.29 vs 2.74 | **1.93** |
| zaur 128k | 9691 / 9748 | 97680 / 97311 | 4.30M / 6.67M | 5.11 vs 3.27 | **1.56** |
| her 96k | 9525 / 10154 | 78889 / 78232 | 1.79M / 3.76M | 6.60 vs 3.11 | **2.12** |
| her 128k | 9534 / 10767 | 112062 / 112414 | 2.72M / 4.51M | 6.16 vs 3.73 | **1.65** |
| naz 96k | 9711 / 10298 | 74236 / 73899 | 1.54M / 3.05M | 6.13 vs 3.09 | **1.99** |
| naz 128k | 9566 / 9762 | 109817 / 109419 | 2.37M / 3.71M | 5.91 vs 3.76 | **1.57** |
| sadie 96k | 11986 / 13236 | 66845 / 64514 | 8.16M / 16.10M | 4.92 vs 2.40 | **2.05** |
| sadie 128k | 11797 / 15532 | 79907 / 73888 | 9.87M / 17.17M | 4.86 vs 2.58 | **1.88** |
| dl8a 96k | 9899 / 10220 | 75451 / 75065 | 1.07M / 2.01M | 5.22 vs 2.77 | **1.89** |
| dl8a 128k | 9770 / 10898 | 106985 / 104859 | 1.59M / 2.36M | 5.00 vs 3.30 | **1.52** |
| hein 96k | 11754 / 13269 | 66934 / 64324 | 8.28M / 16.04M | 4.85 vs 2.41 | **2.02** |
| hein 128k | 11598 / 15672 | 79463 / 73426 | 9.96M / 17.06M | 4.78 vs 2.58 | **1.85** |

Residue totals land within ±4% of ref at matched rate (ratios 0.996–1.082), but
we spend them on 45–57% as many non-zero coefficients at 1.5–2.1× the bits each.
Floor ratio 0.74–0.99 — ref outspends us on floor most where it wins HF (sadie/
hein 128k: ref floor 15.5–15.7k b/s vs our 11.6k).

## Bands summary (per row: 3 largest dNSR bands; E-attenuated bands)

`E −x.x` = encoder output band energy vs source; "attenuated" lists bands with
E_ours ≤ −3 dB while E_ref > −3 dB.

| row | top-3 dNSR bands | attenuated bands | corr ours/ref |
|---|---|---|---|
| nik 96k | 630-770 +11.0; 770-920 +9.8; 400-510 +9.6 | 15500-24000 E −19.0 (ref −2.3) | 0.9690 / 0.9903 |
| nik 128k | 630-770 +12.1; 920-1080 +11.8; 770-920 +11.6 | 15500-24000 E −19.0 (ref −1.8) | 0.9838 / 0.9951 |
| zaur 96k | 510-630 +11.0; 630-770 +10.2; 770-920 +9.5 | none | 0.9739 / 0.9922 |
| zaur 128k | 510-630 +12.5; 630-770 +12.1; 400-510 +10.9 | none | 0.9868 / 0.9955 |
| her 96k | 630-770 +12.1; 400-510 +11.8; 300-400 +11.3 | none | 0.9668 / 0.9867 |
| her 128k | 630-770 +13.0; 400-510 +12.7; 510-630 +12.2 | none | 0.9810 / 0.9927 |
| naz 96k | 510-630 +9.1; 630-770 +8.7; 770-920 +8.7 | 15500-24000 E −3.1 (ref −0.1) | 0.9792 / 0.9923 |
| naz 128k | 630-770 +10.2; 510-630 +10.1; 770-920 +9.9 | none | 0.9914 / 0.9968 |
| sadie 96k | 200-300 +12.2; 510-630 +11.4; 400-510 +10.8 | 12000-15500 E −3.2 (ref +0.6); 15500-24000 E −7.0 (ref −0.1) | 0.9796 / 0.9865 |
| sadie 128k | 200-300 +13.9; 300-400 +12.8; 400-510 +12.8 | 15500-24000 E −5.3 (ref −0.2) | 0.9890 / 0.9906 |
| dl8a 96k | 510-630 +12.7; 630-770 +11.9; 400-510 +11.2 | none | 0.9655 / 0.9916 |
| dl8a 128k | 510-630 +14.6; 630-770 +13.6; 400-510 +12.8 | none | 0.9826 / 0.9957 |
| hein 96k | 510-630 +10.9; 630-770 +10.4; 200-300 +10.3 | 15500-24000 E −6.8 (ref −0.0) | 0.9789 / 0.9869 |
| hein 128k | 510-630 +11.9; 200-300 +11.6; 400-510 +11.6 | 15500-24000 E −5.3 (ref −0.3) | 0.9883 / 0.9906 |

Every row's top-3 dNSR sits in **300–1080 Hz** (dNSR +8.6 to +14.6); in that
region ref reaches NSR −20 to −30 dB while we sit at −8 to −20.

## Diagnosis (numbers only)

(a) **Coding efficiency: yes, every row.** bits/non-zero ours/ref = 1.48–2.12,
all 14 rows ≥ 1.3×; we code 45–57% as many non-zeros (e.g. her 96k 1.79M vs
3.76M) at 4.8–6.6 bits each vs ref's 2.4–3.8, from identical residue budgets.
(b) **HF droop: partial.** nik 15500-24000 E −19.0 dB (effective lowpass at both
rates, ref −1.8/−2.3); naz 96k −3.1; sadie −7.0/−5.3 (plus 12000-15500 −3.2 at
96k); hein −6.8/−5.3. zaur, her, dl8a: no band below −3 dB. (c) **Not flat.**
dNSR is frequency-shaped, not broadband: +9 to +15 at 300–1080 Hz, +2 to +8 at
1–5 kHz, ≤ +1.3 (often negative, we win) above 7.7 kHz; broadband corr gap is
0.0016–0.0131 at 128k and 0.0069–0.0261 at 96k, driven by the low bands.
(d) **Tonal mid 1–5 kHz on her/zaur/dl8a: present but secondary** — her 96k
1080-1270 +6.5 / 1270-1480 +4.7 / 1480-1720 +3.7; zaur 96k 2320-2700 +4.3 /
3150-3700 +3.9; dl8a 96k 2700-3150 +3.7 / 1720-2000 +3.6 — each 5–8 dB below the
same row's 510-770 Hz peak. Conclusion: the dominant gap is (a) residue coding
efficiency concentrated at 300–1080 Hz — ref encodes roughly twice the non-zeros
at half the per-coefficient cost there — with (b) HF droop a separate, source-
dependent contributor (nik/sadie/hein), and (d) minor.

## Deviations from the charter

- Bit-split oracle folded into `band_error_vs_reference` instead of the existing
  12 s × 128k-only `residue_histogram_vs_reference`: same `decode_capture_with_bits`
  + `bit_split_summary` measurement on the same files, one encode pass instead of
  two, and extended to all 14 rows (600 s, both rates).
- Task text said "25 Bark bands" but listed 25 edges; edges taken as authoritative
  → 24 bands (noted in the test's doc comment).
