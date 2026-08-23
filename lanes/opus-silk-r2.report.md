# opus-silk r2 — library gate instrument repair

**Test:** `silk_library_gate_vs_libopus` (`crates/ec-opus/tests/conformance.rs`)
**Source:** `~/Music/sadie.wav`, mono, 120 s cap, 48 kHz, `Application::Voip`, VBR-constrained.
**Scope:** instrument only — no encoder tuning, no fixes, no guesses.

## The four r1 measurement defects (and the r2 repair)

| # | r1 defect | r2 repair |
|---|-----------|-----------|
| 1 | **Decoder asymmetry.** Task premise said "ref decoded by ffmpeg"; the checked-in r1 code decoded the ref via `decode_ogg` (our own `MultistreamDecoder`). Both sides actually used *our* decoder in r1 — the asymmetry description was inverted vs. the code. | Both sides now decode through **ffmpeg libopus** (the reference decoder): ref via `ffmpeg_decode(&ref_ogg)`, ours via `write_ogg_opus` → `ffmpeg_decode(&ours_ogg)`. Own-decoder corr kept as extra column `corr_owndec` so decoder drift stays visible. |
| 2 | **No lag check.** `align_to_source` returned a lag but it was discarded; a lag pinned at the scan bound (±2000) would be a silent invalid measurement. | `align_to_source` return `(lag, aligned)` captured for both sides. **LAG GATE** (hard `assert`): `|lag| < MAX_LAG`. A bound-hit is a failed measurement, not a result. |
| 3 | **Full-band reference.** corr and `opus_compare_err` were computed against the full-band source, penalizing NB/WB encoders for HF energy they structurally cannot reproduce — the likely real cause of the r1 sign contradiction. | Per-row **band-limited reference**: NB → 4 kHz lowpass, WB → 8 kHz lowpass, hybrid-FB → unfiltered (`None`). `corr_bl` (primary) and `opus_compare_err` both use the band-limited source; `corr_fb` (full-band) kept as a secondary column. |
| 4 | **No ref mode.** libopus's actual mode was invisible; "12k-NB" assumed libopus also ran NB. | `first_audio_toc()` reads the ref Ogg, skips OpusHead/OpusTags, returns the first audio packet's TOC byte; `toc_mode_label()` prints it as `ref_mode=`. |

## r1 vs r2 — per row

SIGN RULE (verbatim): `gap = ref_corr - ours_corr; MORE NEGATIVE = OURS BETTER`.
`err_ratio = err_ours / err_ref; < 1 = ours better, > 1 = ref better`.
"agree" = gap and err_ratio point the same way.

| row | | gap | err_ratio | agree? |
|-----|---|-----|-----------|--------|
| 12k-NB | r1 | -0.0308 (ours) | 3.532 (ref) | **no** |
| 12k-NB | r2 | -0.0690 (ours) | 0.061 (ours) | **yes** |
| 16k-NB | r1 | -0.0460 (ours) | 2.573 (ref) | **no** |
| 16k-NB | r2 | -0.1129 (ours) | 0.049 (ours) | **yes** |
| 24k-WB | r1 | -0.0759 (ours) | 4.452 (ref) | **no** |
| 24k-WB | r2 | -0.0940 (ours) | 0.428 (ours) | **yes** |
| 32k-Hyb | r1 | -0.0738 (ours) | 4.343 (ref) | **no** |
| 32k-Hyb | r2 | -0.0738 (ours) | 4.346 (ref) | **no** |

**3 of 4 rows now agree.** The band-limited reference (defect 3) resolved the
contradiction for every NB/WB row. The single remaining disagreement is
**32k-Hyb**, the one row with `cutoff = None` (full-band): `corr_bl == corr_fb`
(0.9279) and `err_ratio` is essentially unchanged from r1 (4.343 → 4.346). At
full band, time-domain corr (dominated by the strong low-frequency energy where
both encoders agree) and frequency-domain `opus_compare_err` (which weights all
bands including HF) genuinely diverge — this is a metric-semantics difference,
not an instrument defect, and is reported as-is (no fix, per scope).

## Lags (defect 2)

| row | lag_ours | lag_ref | LAG GATE |
|-----|----------|---------|----------|
| 12k-NB | -1 | -2 | pass |
| 16k-NB | -1 | -1 | pass |
| 24k-WB |  0 | -1 | pass |
| 32k-Hyb |  0 | -1 | pass |

All lags are 0–2 samples — far inside the ±2000 scan bound. No invalid
measurement. (Sub-sample residual is the encoder pre-skip / look-ahead
quantization to whole frames.)

## Ref modes (defect 4)

| row | ref_mode (libopus) | mode (ours) |
|-----|--------------------|-------------|
| 12k-NB | **SILK-WB** | SILK-NB |
| 16k-NB | **Hybrid-FB** | SILK-NB |
| 24k-WB | **Hybrid-FB** | SILK-WB |
| 32k-Hyb | Hybrid-FB | Hybrid-FB |

libopus does **not** run narrowband at these rates: it auto-selects SILK-WB
(12 k) or full-band hybrid (16/24/32 k). Our encoder honors the forced
SILK-NB / SILK-WB / Hybrid-FB selection. At 12 k and 16 k the comparison is
therefore **NB-ours vs WB/hybrid-ref** — yet ours still wins corr. The
`ref_mode` column makes this mode asymmetry explicit (previously invisible).

## Decoder drift (defect 1, extra column)

`corr_owndec` (our `Decoder`) equals `corr_ours_bl` (ffmpeg libopus) to all
four printed decimals on every row (0.9499, 0.9697, 0.9483, 0.9279). Our
decoder produces bit-identical-quality output to ffmpeg libopus on our own
packets — **no decoder drift**. This also explains why switching the ref from
`decode_ogg` (r1) to `ffmpeg_decode` (r2) barely moved the 32k-Hyb numbers
(corr_ref 0.8541 both; err_ratio 4.343 → 4.346): the two decoders agree.

## Gates

- **RATE GATE** (soft, report-only): all rows within ±5% (max +3.3%).
- **LAG GATE** (hard, new): passed — no lag at scan bound.
- **DROPOUT GATE** (hard): passed — no ours-only dropouts (ref drops *more*
  seconds than ours on every row: 61/69/67/65 vs 14/11/23/32).

## Verification

- `cargo check -p ec-opus --release --test conformance` — clean (0 warnings).
- `silk_library_gate_vs_libopus --ignored --nocapture` — **ok**, 125.5 s,
  sweep written to `lanes/opus-silk-r2.sweep.txt`.
- `cargo test -p ec-opus --release` — **34 + 27 green** (10 ignored), 34.2 s.
