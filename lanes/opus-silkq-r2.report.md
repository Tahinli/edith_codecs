# opus-silkq r2 — where the 12 kbps SILK-WB error lives

Setup: sadie.wav @ 12 kbps target, mono, `Application::Voip`, 120 s, 20 ms frames,
VBR-constrained, both sides decoded through ffmpeg libopus (gate symmetry).
Full tables: `lanes/opus-silkq-r2.bands.txt`.
Reproduce: `cargo test -p ec-opus --release --test conformance silk_spectral_divergence_12k -- --ignored --nocapture`

Headline: err ours=27.421 ref=2.407 **ratio=11.39** (reproduces the gate's ~11×);
rates ours 12.38 kbps vs ref 11.98 kbps (±3%); nframes=47997.

## Mechanism (named)

**Burst-starved constrained-VBR frames + uniform body coarseness — not a
bandwidth/aliasing defect.**

1. **Tail — bit-reservoir starvation bursts.** 141/6000 packets (2.35%) are
   starved to ≤20 bytes (worst: 14–16 B on voiced frames libopus codes at 24–39 B).
   On the worst 24 of these the first-subframe gain index codes **63 (max)** →
   decoder emits a loud mid-band burst (dominant band 2.4–3.8 kHz) where the
   source is quiet: ef2 up to 4.05e27 vs ref 4.5e-8 at the same instant.
   These ~24 packets carry ~100% of ours' Σef2.  Worst clusters: t≈2.78 s
   (pkt 138/139: bytes 46→14, gains[0] 40→63 — spend-then-starve alternation)
   and t≈51.94–52.02 s (pkts 2597–2600: 16 B, gains[0]=63).
2. **Body — uniform elevation.** Every band 0–16 (0–9.6 kHz) shows ours' mean
   eb² at 314×–50 000× ref, tracking signal energy (bands 9–11 hold 62% of
   the share simply because speech energy lives there). dln shows ours' mean
   level *closer* to source than ref's (ref over-attenuates, dln ≈ −0.6…−1.0;
   ours ≈ −0.05…−0.55) — so the error is noise/structure, not level. Consistent
   with r1's systematic first-subframe gain-index offset (ours 33–47 vs ref 26–39):
   over-coarse quantization everywhere, no single band mechanism.
3. **Not aliasing/bandwidth:** bands 17–20 (>9.6 kHz) ratio 1.02–1.10, ours ≈ ref
   (both SILK-WB, nothing up there). Band 16 (8.16–9.6 kHz) ratio 1576× is
   SILK's 8 kHz edge leakage, same order as body bands.

## Table 1 — per-band mean eb² (ours/ref), dln = mean ln(E_test/E_src)

| band | Hz | dln_o | dln_r | eb2_o | eb2_r | ratio | share_o | share_r |
|---|---|---|---|---|---|---|---|---|
| 0 | 0–240 | −0.404 | −1.015 | 9.0e2 | 8.3e−2 | 1.1e4 | 8.1% | 4.5% |
| 1 | 240–480 | −0.555 | −0.822 | 1.5e2 | 3.1e−2 | 4.8e3 | 1.4% | 1.7% |
| 2 | 480–720 | −0.321 | −0.719 | 2.7e2 | 3.1e−2 | 8.9e3 | 2.5% | 1.7% |
| 3 | 720–960 | −0.203 | −0.727 | 2.0e1 | 6.3e−2 | 3.1e2 | 0.2% | 3.4% |
| 4 | 960–1200 | +0.086 | −0.653 | 7.6e2 | 1.3e−1 | 5.9e3 | 6.8% | 6.9% |
| 5 | 1200–1440 | −0.100 | −0.722 | 2.5e2 | 6.1e−2 | 4.0e3 | 2.2% | 3.3% |
| 6 | 1440–1680 | −0.053 | −0.669 | 6.3e1 | 6.2e−2 | 1.0e3 | 0.6% | 3.4% |
| 7 | 1680–1920 | +0.090 | −0.590 | 6.5e1 | 1.4e−1 | 4.8e2 | 0.6% | 7.4% |
| 8 | 1920–2400 | −0.061 | −0.686 | 6.8e2 | 1.7e−1 | 4.1e3 | 6.1% | 9.0% |
| **9** | **2400–2880** | −0.053 | −0.685 | 3.5e3 | 7.0e−2 | **5.0e4** | **31.5%** | 3.8% |
| **10** | **2880–3360** | −0.159 | −0.713 | 2.0e3 | 1.4e−1 | **1.4e4** | **17.9%** | 7.5% |
| **11** | **3360–3840** | −0.193 | −0.711 | 1.4e3 | 1.1e−1 | **1.3e4** | **13.0%** | 5.9% |
| 12 | 3840–4800 | −0.090 | −0.640 | 7.5e2 | 1.2e−1 | 6.1e3 | 6.8% | 6.7% |
| 13 | 4800–5760 | −0.377 | −0.734 | 1.4e2 | 1.6e−1 | 8.5e2 | 1.2% | 8.6% |
| 14 | 5760–6720 | −0.345 | −0.685 | 5.0e1 | 7.8e−2 | 6.4e2 | 0.5% | 4.2% |
| 15 | 6720–8160 | −0.203 | −0.594 | 3.4e1 | 7.7e−2 | 4.5e2 | 0.3% | 4.1% |
| 16 | 8160–9600 | +0.074 | −0.366 | 5.1e1 | 3.3e−2 | 1.6e3 | 0.5% | 1.8% |
| 17 | 9600–11520 | −1.565 | −1.530 | 1.4e−1 | 1.3e−1 | **1.08** | 0.0% | 7.2% |
| 18 | 11520–14400 | −1.910 | −1.910 | 1.3e−1 | 1.2e−1 | **1.10** | 0.0% | 6.6% |
| 19 | 14400–18720 | −1.103 | −1.104 | 4.7e−2 | 4.6e−2 | **1.02** | 0.0% | 2.5% |
| 20 | 18720–24000 | −0.046 | −0.046 | 3.9e−5 | 3.8e−5 | **1.02** | 0.0% | 0.0% |

## Table 2 — worst frames (top of 15; full table in bands.txt)

| t(s) | pkt | ef2_ours | ef2_ref | dom band | ours: B, g0 | ref: B, g0 |
|---|---|---|---|---|---|---|
| 2.780 | 139 | 4.05e27 | 4.5e−8 | 2.4–2.9k | **14, 63** | 37, 23 |
| 2.777 | 138 | 5.49e26 | 5.8e−8 | 2.4–2.9k | 46, 40 | 39, 21 |
| 2.783 | 139 | 2.96e26 | 8.3e−8 | 2.4–2.9k | **14, 63** | 37, 23 |
| 51.998 | 2599 | 2.71e24 | 3.1e−9 | 1.9–2.4k | 44, 44 | 30, 26 |
| 52.005 | 2600 | 2.00e24 | 1.9e−9 | 1.0–1.2k | **16, 63** | 32, 24 |
| 52.000–52.015 | 2600 | 1.2e23–1.7e24 | ~e−9 | 1.0–1.9k | **16, 63** | 32, 24 |
| 51.943 | 2597 | 2.89e20 | 2.3e−11 | 1.2–1.4k | **16, 63** (lag 200) | 24, 29 |

Oracle rows (sig/qoff/nlsf_i/lag/cont/per/ltp) in bands.txt — both sides code
these frames voiced (sig=2, qoff=0, nlsf_i=4); ours differs in bytes, g0, and
contour (ours cont=3 everywhere vs ref 0/1/5/12) — contour bookkeeping is a
secondary suspect, but bytes+g0 is the load-bearing difference.

## Table 3 — tail vs body

- top-1% (479 frames) carry 100.00% of Σef2 **both sides** (ef² is squared
  twice before summing — the metric is tail-dominated by construction).
- de-tailed (worst 50 frames dropped per side): err ours=5.624 ref=0.812 →
  **ratio 6.93** (was 11.39). Bursts alone explain roughly half the log-scale gap.
- burst signature: packets with gains[0]≥60: **24/6000**, mean ef2 2.27e25
  (vs 1.15e22 for the rest, ≈2000×); packets ≤20 bytes: **141/6000**, mean
  ef2 3.86e24 (vs 1.18e22, ≈330×).
- [INFERENCE] If the ≤20 B starvation were eliminated (bursts → body quality):
  ours' err ≈ 5.624 vs unclipped ref 2.407 → **ratio ≈ 2.3**, a −80% drop —
  far past the gate KEEP threshold (−20%).

## Fix decision

**No encoder change this round.** The table names the mechanism but does not
identify a ≤10-line port: the candidate libopus logic (voiced-frame minimum-bit
floor / constrained-VBR reservoir clamp in silk VBR bit assignment) is not
present locally (only opus headers installed) and porting rate-control from
memory is off-limits. r3 pointer: fetch libopus `silk/VBR.c` +
`encode_frame` bit assignment, find the per-frame min/clamp, port behind
`const ON`, gate, SIGN RULE. Expected effect per the inference above.

## Verification

- `cargo check -p ec-opus --tests` — clean.
- `silk_spectral_divergence_12k` (release, --ignored): ok, 29.5 s, wrote
  `lanes/opus-silkq-r2.bands.txt` (75 lines).
- `cargo test -p ec-opus --release`: **34 + 27 passed, 0 failed** (13 ignored
  diagnostics, incl. the new one).
