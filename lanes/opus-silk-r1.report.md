# opus-silk r1 — SILK / speech-rate library gate vs libopus

Lane `lane-opus-silk`, run 2026-08-23. Measures our `ec-opus` SILK/speech-rate
encoder against ffmpeg libopus on real speech (`~/Music/sadie.wav`), MONO, 48 kHz,
`Application::Voip`, VBR constrained, 120 s cap.

- **Source:** `~/Music/sadie.wav` downmixed to mono (`-ac 1 -ar 48000`), 120 s cap.
- **Ours:** `Encoder::new(48000, 1, Application::Voip)`, `set_vbr_constrained(true)`,
  `set_bitrate` to the reference's realised rate; `set_bandwidth`/`set_mode` per row so
  Voip actually codes SILK/Hybrid (mode printed from the first packet's TOC config byte,
  RFC 6716 §3.1).
- **Reference:** `ffmpeg libopus -application voip -b:a <rate>k -vbr on`, mono.
- **SIGN RULE:** `gap = ref_corr - ours_corr`; **more negative = ours better**.
- **RATE GATE** ±5% (ours_kbps vs ref_kbps); **DROPOUT GATE** ours must not drop
  seconds the reference kept.
- **Run:** `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-opus-sb cargo test -p ec-opus --release --test conformance silk_library_gate_vs_libopus -- --ignored --nocapture`

## Gates

- **RATE GATE:** all rows within ±5%. PASSED (reported, not asserted).
- **DROPOUT GATE:** passed (no ours-only dropouts). PASSED (asserted).

## Results

| row      | mode       | ours kbps | ref kbps | rate %  | corr ours | corr ref | gap     | Q ours  | Q ref  | err_ratio | minsec ours | minsec ref | drop ours | drop ref |
|----------|------------|-----------|----------|---------|-----------|----------|---------|---------|--------|-----------|-------------|------------|-----------|----------|
| 12k-NB   | SILK-NB    | 12.4      | 12.0     | +3.3    | 0.8489    | 0.8181   | -0.0308 | -821.15 | -401.54| 3.532     | 0.2408      | 0.2079     | 67        | 69       |
| 16k-NB   | SILK-NB    | 16.8      | 16.4     | +2.4    | 0.8665    | 0.8205   | -0.0460 | -725.78 | -416.74| 2.573     | 0.2420      | 0.2861     | 63        | 73       |
| 24k-WB   | SILK-WB    | 24.4      | 24.0     | +1.7    | 0.9168    | 0.8409   | -0.0759 | -861.98 | -367.31| 4.452     | 0.5591      | 0.3300     | 40        | 71       |
| 32k-Hyb  | Hybrid-FB  | 31.6      | 31.8     | -0.5    | 0.9279    | 0.8541   | -0.0738 | -939.95 | -434.68| 4.343     | 0.6284      | 0.3680     | 33        | 65       |

### Mode per row (actual, from TOC config byte)

- **12k-NB** → `SILK-NB` (config 0–3). Auto: per-channel 12k ≤ 12k → SILK NB (8 kHz). No forcing needed.
- **16k-NB** → `SILK-NB` (config 0–3). Forced `Bandwidth::Narrow`; without forcing, 16k would auto-pick SILK-MB (12 kHz).
- **24k-WB** → `SILK-WB` (config 8–11). Forced `Mode::Silk` + `Bandwidth::Wide`; without forcing, 24k ≥ 20k would leave SILK and `hybrid_choice` would pick Hybrid-SWB.
- **32k-Hyb** → `Hybrid-FB` (config 14–15). Auto: per-channel 32k ∈ 20k..40k → Hybrid; `auto_bandwidth` equiv = 32k×0.7 = 22.4k ≥ 20k → Full. No forcing needed.

## Three worst per-row observations (measured only)

1. **err_ratio peaks at 24k-WB (4.452).** Despite ours winning on aggregate
   correlation (`gap = -0.0759`, the best margin of any row), the `opus_compare`
   error metric is 4.45× the reference's — the worst err_ratio of the table.
   32k-Hyb is close behind (4.343). The two higher-bandwidth rows (WB, Hybrid-FB)
   carry the largest err_ratio, while the NB rows sit lower (2.573–3.532). Ours
   beats ref on correlation everywhere yet loses on the compare-error metric
   everywhere.

2. **Per-second correlation floor is lowest at 12k-NB (`minsec` ours = 0.2408).**
   The single weakest second across all rows; 16k-NB is nearly as low (0.2420).
   The NB rows have the shallowest per-second corr floor even though aggregate
   corr is respectable (0.85–0.87). Notably the reference's 12k floor is even
   lower (0.2079), so ours is not the absolute worst here, but 12k-NB is where
   the encoder is most second-to-second fragile.

3. **Dropout seconds are highest at 12k-NB (ours 67, ref 69).** The most seconds
   below the 0.9 corr threshold for both encoders; counts fall monotonically with
   rate (ours 67 → 63 → 40 → 33). The DROPOUT GATE passes only because the
   reference drops *at least as many* seconds in every row (ref 69, 73, 71, 65) —
   ours actually drops fewer seconds than ref in all four rows, but the absolute
   count is high at the low rates.

### Rate budgeting note (measured, no fix)

Ours overshoots the reference's realised rate on the three SILK rows
(+3.3%, +2.4%, +1.7%) and undershoots on the hybrid row (-0.5%). All within the
±5% RATE GATE. Consistent with the encoder's known state: SILK packets are not
yet budgeted against the target rate (encoder.rs "D2b"). Reported as observed;
no tuning performed this lane.
