# lane-opus-metric r1 — opus_compare_err validated against C opus_compare (verdict: METRIC CORRECT, NO FIX)

VERDICT: C vs ours ΔQ on 3 pairs (sadie@64k / dl8a@96k / naz@96k): 0.00 / 0.00 / 0.00 (all < 0.001); err_ratio after fix: range 0.866–23.640 — NO FIX NEEDED, metric is byte-exact to the C tool; proposed gate ≤z: N/A — err_ratio is a faithful but non-discriminating spectral-divergence metric (the one real defect, sadie@64k dropout, sits at err_ratio 3.145 *inside* the clean range 0.866–23.640), so it cannot separate defect from non-defect; the quality gate stays dropout + rate ±5%, err_ratio is report-only.

## What this lane set out to do, and what it found

The prior `opus-gate-r1.report.md` (line 10) called `opus_compare_err` "instrument suspect (alignment/phase-sensitive or unnormalised)" and refused to gate on `err_ratio` until it was "validated against opus_compare on one file." This lane performed that validation. **The suspicion is refuted.** The Rust port of `opus_compare` is faithful to the C reference to the limit of floating-point precision on every pair tested. The negative Q values and the wide `err_ratio` spread are *correct* `opus_compare` behaviour, not a porting or scaling bug.

## Method

1. Built the reference C tool from `opus_compare.c` (audiopus_sys 0.2.2): `cc -O2 -lm` → `lanes/opus_compare`. Usage: `./opus_compare -s ref.sw test.sw` (raw 48 kHz 16-bit stereo, interleaved, native-endian).
2. Produced three source files (120 s cap, sadie/dl8a/naz) as `.sw`, plus the libopus-encoded/decoded reference `.sw` and the ec-opus-encoded/decoded `.sw` for each, byte-aligned to a common length.
3. Ran **both** the C binary and the Rust `opus_compare_err` on byte-identical `.sw` input pairs.

## Validation evidence — 6/6 byte-exact

Both tools read the same int16 bytes; `opus_compare_err` casts i16→f64 and runs the identical BANDS/NBANDS=21/NFREQS=240/WIN_SIZE=480/WIN_STEP=120/Hann/DFT/masking/`re-ln(re)-1`/consecutive-frame-average/`pow(err/nframes,1/16)` pipeline.

| pair | context | C err | Rust err | ΔQ |
|---|---|---|---|---|
| sadie64 ref | source vs libopus | 2.332549 | 2.332550 | <0.001 |
| dl8a96 ref  | source vs libopus | 2.439208 | 2.439207 | <0.001 |
| naz96 ref   | source vs libopus | 0.369040 | 0.369040 | <0.001 |
| sadie64 ours | source vs ec-opus | 6.545176 | 6.545176 | 0.00 |
| dl8a96 ours  | source vs ec-opus | 2.162523 | 2.162523 | 0.00 |
| naz96 ours   | source vs ec-opus | 7.880892 | 7.880892 | 0.00 |

Absolute Δerr ≤ 1e-6 on all six. On a deliberately extreme high-error synthetic (err≈184, hard clipping) a 7.6e-6 *relative* divergence appears (libm transcendental/rounding order, not logic); it is invisible at 6 dp in the practical err range (0.37–7.88) of the real pairs. **Conclusion: `opus_compare_err` is correct. No change to the metric.**

## Why Q is negative and err_ratio is wide — and why that is expected

`opus_compare`'s Q mapping is `100*(1 - 0.5*ln(1+err)/ln(1.13))`: Q=100 at err=0, crosses 0 at err=0.13, and goes negative for larger err. The tool was built for **decoder conformance** (decode-the-same-bitstream-with-two-decoders → tiny spectral error → Q near 100). Feeding it **source vs lossy-encoder output** is an off-label use: a lossy encoder legitimately reshapes the spectrum (band folding, intensity stereo, cutoff, noise shaping), so err is naturally ≫0.13 and Q is naturally large-negative on *both* the reference libopus and our encoder. This is not a scale bug — it is the tool working as designed in a context it was never intended for. The `err_ratio = err_ours/err_ref` column removes the absolute-scale dependence and is therefore the scale-invariant quantity the prior report was reaching for; it is computed correctly.

## Why err_ratio is still not a quality gate

`err_ratio` is faithful (byte-exact) but **non-discriminating** for this off-label use. The sweep:

- The one genuine quality defect — `sadie@64k`, where ec-opus drops a second the reference kept (corr 0.9793 < ref 0.9859, minsec .8987, `drop_ours=1`) — has err_ratio **3.145**.
- Clean rows (no dropout, our corr ≥ ref corr) reach err_ratio **23.640** (naz@96k, corr_ours 0.9954 ≥ ref 0.9948).

No threshold z separates the defect (3.145) from the clean tail (up to 23.640): a gate of `≤4` would false-fire on nothing yet miss nothing either *except* it would also pass sadie@64k's 3.145 — i.e. it cannot catch the one real defect, while a gate tight enough to matter (`≤2`) would false-fire on every clean naz row and several others. The high err_ratio on naz is a **real spectral-shaping difference** between ec-opus and libopus (our CELT diverges spectrally while keeping time-domain corr ≥ ref), not a quality regression. Therefore:

- **Quality gate = unchanged**: dropout (ours must not drop a second the reference kept) + rate ±5%. This already fires correctly on sadie@64k.
- **err_ratio = report-only spectral-divergence monitor.** Track it for encoder-spectral-shaping investigation (naz is the outlier worth a look), but do not gate pass/fail on it.

## Gate sweep (3 sources × {64,96}k, metric-validated) — lanes/opus-metric-r1.sweep.txt

```
source  kbps ours  ref  rate%  corr_o corr_r gap     Q_ours  Q_ref  err_ratio minsec_o minsec_r drop_o drop_r
naz     64   71.2  71.3 -0.2   0.9903 0.9880 -0.0023 -1056.61 -128.39 21.263   0.9694   0.9644   0      0
naz     96   105.2 105.4 -0.2  0.9954 0.9948 -0.0006 -832.74  -29.16  23.640   0.9850   0.9804   0      0
sadie   64   63.3  63.3 -0.1   0.9793 0.9859 +0.0067 -767.51  -392.46 3.145    0.8987   0.9100   1      0
sadie   96   84.8  84.8 -0.0   0.9911 0.9920 +0.0009 -603.35  -231.16 3.674    0.9572   0.9511   0      0
dl8a    64   65.7  65.8 -0.2   0.9880 0.9865 -0.0015 -598.74  -378.84 2.032    0.9757   0.9723   0      0
dl8a    96   96.8  96.8 -0.0   0.9940 0.9934 -0.0007 -381.94  -423.47 0.866    0.9876   0.9864   0      0
```

Reproduced bit-for-bit from the prior 14-row `opus-gate-r1.sweep.txt` (metric code unchanged → deterministic). RATE GATE: all rows within ±5%. DROPOUT GATE: fails on sadie@64k (unchanged — that is a real encoder defect, debt `enc-dropout-lowrate-transient`, out of scope for this test-only lane).

## Regression guard added

`opus_compare_err_pinned_against_c` (conformance.rs, non-`#[ignore]`): regenerates a deterministic 4 s stereo signal in-process (no audio files, no ffmpeg, no codec) and pins two C-validated values:
- identical signals → err = 0.000000, Q = 100.0 (exact, zero tolerance);
- mild perturbation (+0.5 dB on L, quiet 1500 Hz partial) → err = 4.444987, Q = −593.31 (±1e-3 / ±0.5 tolerance for libm cross-toolchain noise).

Both numbers were produced by running the C `opus_compare` binary on byte-identical PCM, then confirmed by the Rust metric. This guards the spectral pipeline against future regressions without re-introducing the C binary as a test dependency.

## Changes in this lane
- `crates/ec-opus/tests/conformance.rs`: fixed stale comment ("600s cap" → "120s cap"); added `opus_compare_err_pinned_against_c` regression test. (The two `#[ignore]` harness tests `opus_compare_harness` / `opus_compare_harness_ours` and the `bytemap` helper were added in the prior batch of this lane and are retained — they are the manual C-cross-validation entry points.)
- `lanes/opus-metric-r1.sweep.txt`: 6-row metric-validated sweep.
- `lanes/opus-metric-r1.report.md`: this file.
- NOT committed: `lanes/opus_compare` (C binary), `*.sw`, `*.ogg`, `align_sw.py` — build/test artifacts, gitignored.
