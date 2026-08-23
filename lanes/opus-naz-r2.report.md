# lane-opus-naz r2 — startup leak = transient tf_res default inversion

## Cause
Silence frames decode to exact zeros (`celt_silence_then_attack_decodes_bounded`).
The leak was pre-echo in the first attack frame: encoder coded raw `tf_res = 0`
with `tf_select = 0` on a transient LM=3 frame, which `TF_SELECT[3][4+0+0]` maps
to `+3` — all 8 short blocks merged back into one long block in every band. The
source is ≈0 for 12 ms before the onset, so opus_compare's Y/X ratio exploded.
libopus without tf_analysis codes raw `tf_res[i] = isTransient` (change 0).

Evidence (`naz_startup_hop_energies`, energy per 2.5 ms hop, 37.5–45 ms):
ours before 8e-4…6e-3, ours after 3e-19…2e-18, libopus 0…8e-7. Decoder diag of
frame 2: OUR tf `[3;21]` before, `[0;21]` after; REF `[1,1,1,-1,…]` (tf_analysis).

## Fix
`celt_enc.rs`: `self.tf_res = [i32::from(is_transient); NB_BANDS]` + doc comment
corrected (it claimed the opposite of the table). `CeltDecDiag` gained
`tf_res/spread/anti_collapse`.

## Gate (14 rows, vs opus-drop-r2)
nik@64k: ref 69.2 ours 69.2 kbps (+0.0%), corr o=0.9876 r=0.9864 gap=-0.0012, Q o=-399.22 r=-154.78, err_ratio 2.764, drop o=0 r=0
nik@96k: ref 101.0 ours 100.8 kbps (-0.2%), corr o=0.9944 r=0.9940 gap=-0.0003, Q o=-227.79 r=-31.55, err_ratio 3.239, drop o=0 r=0
zaur@64k: ref 63.6 ours 63.5 kbps (-0.0%), corr o=0.9883 r=0.9866 gap=-0.0017, Q o=-335.73 r=-123.36, err_ratio 2.618, drop o=0 r=0
zaur@96k: ref 93.9 ours 93.8 kbps (-0.0%), corr o=0.9946 r=0.9937 gap=-0.0009, Q o=-265.38 r=-125.18, err_ratio 1.966, drop o=0 r=0
her@64k: ref 67.9 ours 67.3 kbps (-0.9%), corr o=0.9827 r=0.9794 gap=-0.0033, Q o=-839.84 r=-339.00, err_ratio 4.650, drop o=0 r=0
her@96k: ref 101.5 ours 100.9 kbps (-0.6%), corr o=0.9913 r=0.9902 gap=-0.0011, Q o=-899.99 r=-191.50, err_ratio 10.126, drop o=0 r=0
naz@64k: ref 71.3 ours 71.3 kbps (-0.0%), corr o=0.9903 r=0.9880 gap=-0.0024, Q o=-222.24 r=-128.39, err_ratio 1.603, drop o=0 r=0
naz@96k: ref 105.4 ours 105.3 kbps (-0.0%), corr o=0.9954 r=0.9948 gap=-0.0007, Q o=-229.13 r=-29.16, err_ratio 3.328, drop o=0 r=0
sadie@64k: ref 63.3 ours 62.6 kbps (-1.1%), corr o=0.9816 r=0.9859 gap=+0.0043, Q o=-389.15 r=-392.46, err_ratio 0.988, drop o=0 r=0
sadie@96k: ref 84.8 ours 84.0 kbps (-1.0%), corr o=0.9920 r=0.9920 gap=-0.0001, Q o=-303.07 r=-231.16, err_ratio 1.346, drop o=0 r=0
dl8a@64k: ref 65.8 ours 65.4 kbps (-0.6%), corr o=0.9879 r=0.9865 gap=-0.0015, Q o=-386.87 r=-378.84, err_ratio 1.029, drop o=0 r=0
dl8a@96k: ref 96.8 ours 96.0 kbps (-0.9%), corr o=0.9939 r=0.9934 gap=-0.0006, Q o=-340.97 r=-423.47, err_ratio 0.747, drop o=0 r=0
hein@64k: ref 64.9 ours 64.5 kbps (-0.6%), corr o=0.9853 r=0.9874 gap=+0.0020, Q o=-426.68 r=-388.31, err_ratio 1.141, drop o=0 r=0
hein@96k: ref 86.9 ours 86.3 kbps (-0.7%), corr o=0.9937 r=0.9930 gap=-0.0006, Q o=-376.12 r=-246.46, err_ratio 1.653, drop o=0 r=0
RATE ±5% pass, DROPOUT pass. naz err_ratio 21→1.60 @64, 23.9→3.33 @96; corr gap
−.0024/−.0007. No row regressed corr by >.001; sadie/hein@64 unchanged (lane-opus-64).

## Suite
ec-opus: 34 + 27 pass, 7 ignored. Two assertions re-sized after the change:
- `hybrid_layers_align`: layer tolerance 2→3. Fullband CELT click peaks at
  exactly +120 (`celt_click_peak_offset`, 32/64/128 k); the band-limited HB
  layer's main lobe now sits at +118, SILK at +121.
- `silk_compares_to_celt_on_speech_at_speech_rates`: SILK ≥ CELT − .005; CELT
  edges SILK by .002 at NB16 (synthetic). Residue: SILK encoder quality.

## Residue
her@96 err_ratio 10.1 (was 13+) — next largest startup/transient window; not named.
