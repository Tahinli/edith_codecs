# lane opus-mode r1 — libopus VoIP/Audio mode + bandwidth selector — KEEP

## What
- `encoder.rs`: mode/bandwidth decision is now libopus 1.6's: `equiv_rate = bitrate·(90+complexity)/100` with the 10 ms frame-rate term, `mode_thresholds` blended by `voice_est` (VoIP 127, Audio 48 with analysis off → SILK below ≈63.2k / 17.6k), `MONO/STEREO_{VOICE,MUSIC}_BW` tables (pairs of threshold/width, NB↔MB 9000, WB↔SWB 13500/11000, SWB↔FB 14000/12000) blended by `voice_est²`, SILK-with-bandwidth>WB → Hybrid, 40/60 ms frames capped to SILK-WB, Fs ceiling. Old hand thresholds removed.
- Unit tests + the 24-case `encoder_mode_follows_...` table re-derived from the actual run (18 cases changed: e.g. VoIP 12k → SILK-WB not NB, 16k → Hybrid-FB, Audio 16k → Hybrid-FB). Builder's hand table had 8k-Audio wrong (is NB); actual run is the source.
- `silk_library_gate_vs_libopus`: rows now all-auto, full-band, with a MODE GATE assert (ours must pick the reference's mode).

## Gate (real speech, mono VoIP, 120 s; gap = ref − ours, negative = ours ahead)
```
12k: ref_mode=SILK-WB mode=SILK-WB, ref 12.0 ours 12.4 kbps (+3.3%), lag o=0 r=-2, corr_bl o=0.8081 r=0.8181 gap=+0.0101, corr_fb o=0.8081 r=0.8181, corr_owndec o=0.8081, Q o=-1269.32 r=-401.54, err_r
16k: ref_mode=Hybrid-FB mode=Hybrid-FB, ref 16.4 ours 18.6 kbps (+12.9%), lag o=0 r=-1, corr_bl o=0.8547 r=0.8206 gap=-0.0341, corr_fb o=0.8547 r=0.8206, corr_owndec o=0.8540, Q o=-1032.96 r=-416.69, 
24k: ref_mode=Hybrid-FB mode=Hybrid-FB, ref 24.0 ours 24.3 kbps (+1.1%), lag o=0 r=-1, corr_bl o=0.8964 r=0.8409 gap=-0.0555, corr_fb o=0.8964 r=0.8409, corr_owndec o=0.8965, Q o=-1032.13 r=-367.32, e
32k: ref_mode=Hybrid-FB mode=Hybrid-FB, ref 31.8 ours 31.6 kbps (-0.5%), lag o=0 r=-1, corr_bl o=0.9279 r=0.8541 gap=-0.0738, corr_fb o=0.9279 r=0.8541, corr_owndec o=0.9279, Q o=-940.18 r=-434.68, er
RATE GATE violations (|rate%| > 5):
LAG GATE: passed (no lag at scan bound)
DROPOUT GATE: passed (no ours-only dropouts)
```
MODE GATE: 4/4 match. Like-for-like residue now visible: 12k SILK-WB ours behind on both metrics (gap +.0101, err_ratio 11.4); 16k hybrid rate +12.9% (RATE GATE is report-only — SILK part unbudgeted, encoder.rs D2b); 24k/32k hybrid corr ahead (−.056/−.074) but err_ratio 7.0/4.3 (metric split, see debt).

## Suite
test result: ok. 34 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.02s
test result: ok. 27 passed; 0 failed; 10 ignored; 0 measured; 0 filtered out; finished in 34.14s
14-row CELT gate unaffected by construction: Audio at 64/96k is far above the 17.6k Audio SILK threshold (CELT both before and after).
