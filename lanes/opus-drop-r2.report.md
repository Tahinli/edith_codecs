# opus-drop r2 — sadie@64k dropout second

**VERDICT: sadie@64 minsec .8987→.9148, drops 1→0; 14-row: 12 rows corr≥ref; worst regression row her@64 corr −.0002 (gap −0.0033, still ≥ref)**

Full sweep: `lanes/opus-drop-r2.sweep.txt` (baseline: committed `lanes/opus-gate-r1.sweep.txt`). Rate gate: all rows within ±5% (worst sadie@64 −1.1%). Dropout gate: 0 ours-only dropout seconds on all 14 rows.

## Root cause (r2 finding, reverses r1 plan)

The failing second (53; frames 2656-2705) was a **reservoir arithmetic** failure, not a floor_depth failure:

- Ref (ffmpeg `-vbr on`) = **unconstrained** VBR: libopus celt's constrained-VBR reservoir/drift/bailout block (`celt_encoder.c:2509-2529`) is skipped entirely. Ref's quiet-frame spend (76-87B) comes from `compute_vbr()`'s **activity cut**: `if (activity < .4) target -= (coded_bins<<BITRES)*(.4-activity)` with tonality-analysis activity≈0 in silence → −44.8B at coded_bins=896.
- r1's hypothesis (floor_depth binding on quiet frames) was wrong: quiet frame 2661's bands sit ≈13dB above the noise floor; floor_depth binds only below maxDepth ≈ +1.3dB.
- Ours spent flat 158B (rate) through the quiet run at reservoir 0; the transient run (2685-2691) then accrued +10056 eighth-bits of debt, and the two loudest frames (2692/2693, rms .047/.066) starved at 136/135B vs ref 252/159B → min-second corr .8987.

## Fix (one lever: `crates/ec-opus/src/celt_enc.rs` VBR block, :721-845)

Replaced the tf ladder with libopus `compute_vbr` shaping plus credit banking:

1. **Activity proxy** (the lever; we run no tonality analysis): `peak_db = 3.0103 · max(band_log_e[i+ch·NB_BANDS] + E_MEANS[i])` over ch,i<end; `activity = clamp01((peak_db − 5)/20)` — silence≈0, music≈1 (calibrated on dump frames 2661→0, 2658→1). Below .4: `target -= coded_bins·8·(.4−activity)` with `coded_bins = (E_BANDS[end−1] + E_BANDS[intensity_est]) << lm` (= 896 at 64k stereo, intensity_est recomputed with the stereo section's thresholds).
2. **tot_boost term** (faithful): `target += total_boost − (19<<lm)`.
3. **tf term**: transient keeps the 7/4 boost (stand-in for tf_estimate boost); steady frames get `+0.012·target` (tf_estimate_eff = 0.05 vs libopus bias 0.044).
4. **floor_depth cap** (faithful, near-total-silence safety): noise floor from LOG_N/E_MEANS in our log2 units; `target = min(target, max(C·bins·8·3.0103·maxDepth, target>>2))`.
5. **2·base_target cap** (faithful).
6. **Capped bailout**: reservoir debt releases ≤8B/frame into nb_avail (libopus CVBR re-adds everything next frame — would defeat banking).
7. **Credit floor**: `vbr_reservoir ≥ −8·vbr_rate`.
8. Early clamp (:505-517) untouched — with banked credit it lets transient/post-attack frames spend ~250B like ref.

Quiet frames now target ~90-100B, banking credit the attack spends; 2692/2693 no longer starve.

## Deviations from libopus (documented)

- activity from peak-band-level proxy instead of TonalityAnalysis (no analysis pass in this encoder)
- tf from transient flag + fixed 0.05 estimate instead of tf_analysis
- tonality boost, stereo_saving term, temporal_vbr, CVBR 0.67 damping: skipped (no analysis data)
- lsb_depth hardcoded 24 in the noise-floor constant

## Numbers

| row | corr base→r2 | gap r2 | note |
|---|---|---|---|
| sadie 64 | .9793→**.9816** | +.0043 | drops 1→0, minsec .8987→.9148 |
| sadie 96 | .9911→**.9921** | −.0001 | flipped to ours≥ref; minsec .9572→.9661 |
| hein 64 | .9841→**.9854** | +.0020 | gap narrowed +.0033→+.0020 |
| her 64 | .9829→.9827 | −.0033 | worst regression −.0002 |
| other 10 | ±.0002 | ours≥ref | unchanged |

Rate: all rows −1.1%…+0.0% vs ref.

Instrumentation test `sadie64_dropout_instrumentation` removed (unattended default); `CeltFrameDiag`/`last_celt_diag` diag plumbing kept (pub API). Evidence: gate runs above; dump analysis in `lanes/opus-drop-r1.second.txt`.
