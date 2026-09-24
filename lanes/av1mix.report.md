# lane-av1-mixloss : rapport de session (24-09-2026, session Mert, r1)

## État livré
- WIP commit **4d5b5397** sur `lane-av1-mixloss` (base 7404c36a). Non pushé, main non touché.
- Le lift per-segment lossless est câblé de bout en bout (reprise du travail d'avant la rate limit) :
  - `FrameCtx::lossless_per_seg` (table 8 segments, set depuis `stream.rs` via `set_lossless_per_seg`)
  - `decode::lossless()` résout par le `cur_segment_id` du bloc (l'ordre segment-id AVANT tx-geometry,
    corrigé en session précédente, est en place et documenté)
  - `read_tx_size` : TX_4X4 en première ligne (libaom decodeframe.c:1203)
  - intrabc var-tx tree gated sur `!lossless(fctx)` (decode_block + decode_leaf8)
  - refusal "frame mixing lossless and lossy segments" levé dans stream.rs + refusal_inventory
  - witness gate réécrite : `a_real_aomenc_mixed_lossless_segment_frame_decodes_sample_exact`
    (3 arms aomenc, WHT-unit counter + comparaison sample-exact ffmpeg)
  - compteur `LOSSLESS_WHT_UNITS` ajouté (proof of run)
- Le décode réussit sur les 4 frames du mix (256x192, testsrc2/gradients, aomenc --aq-mode=1 --cq-level=0).

## Diagnostic de la divergence (session)
- Ours vs ffmpeg diverge : Y premier pixel faux à **(40,0)** ; U/V à **(16,0)** (= luma (32,0)).
- **Alignement exact de la chaîne rng EC_COEFF avec l'oracle aom sur les TUs 0-18** (luma + chroma),
  y compris les trails eob/level/sign/golomb complets (valeurs ET rng identiques). Le parse est sain.
- Le fork précis est **TU#19 = luma (40,0)** : au même in-rng 50864, même ctx txb_skip (3), mais le CDF
  diffère (notres cdf0=23340 vs oracle 10886) ⇒ pas un désync de parse, une **dérive de l'état CDF**
  (classe `cdf-state-drift`, famille av1-cdf-convention-mirror).
- Adressé : EC_PRED trace ; les TUs 0-18 alignés. Piste : la trace ISTEP.
- Adressé : la divergence entre notres dequant q et l'oracle a déjà émergé en session précédente.

## Diagnostic de la divergence (session)
- Ours vs ffmpeg diverge : Y premier pixel faux à **(40,0)** ; U/V à **(16,0)** (= luma (32,0)).
- **Alignement exact de la chaîne rng EC_COEFF avec l'oracle aom sur les TUs 0-18** (luma + chroma),
  y compris les trails eob/level/sign/golomb complets (valeurs ET rng identiques). Le parse est sain.
- Le fork précis est **TU#19 = luma (40,0)** : au même in-rng 50864, même ctx txb_skip (3), mais le CDF
  diffère (notres cdf0=23340 vs oracle 10886) ⇒ pas un désync de parse, une **dérive de l'état CDF**
  (classe `cdf-state-drift`, famille av1-cdf-convention-mirror).

## Diagnostic de la divergence (session)
- Ours vs ffmpeg diverge : Y premier pixel faux à **(40,0)** ; U/V à **(16,0)** (= luma (32,0)).
- **Alignement exact de la chaîne rng EC_COEFF avec l'oracle aom sur les TUs 0-18** (luma + chroma),
  y compris les trails eob/level/sign/golomb complets (valeurs ET rng identiques). Le parse est sain.
- Le fork précis est **TU#19 = luma (40,0)** : au même in-rng 50864, même ctx txb_skip (3), mais le CDF
  diffère (notres cdf0=23340 vs oracle 10886) ⇒ pas un désync de parse, une **dérive de l'état CDF**
  (classe `cdf-state-drift`, famille av1-cdf-convention-mirror).

## Pistes pour la prochaine session
1. Le lead est le **stockage des données CDF** : les TUs 0-18 alignés, Piste : la trace ISTEP.
- Vérifier : la divergence entre notres dequant q et l'oracle a déjà émergé en session précédente.

## Pistes pour la prochaine session
1. Le lead est le **stockage des données CDF** : les TUs 0-18 alignés, Piste : la trace ISTEP.

## Notes
- Le décode réussit sur les 4 frames du mix (256x192, testsrc2/gradients, aomenc --aq-mode=1 --cq-level=0).

## Notes de session (traces)
- Tout dans `~/.cache/tmp-av1mix/` : `oraclesteps.txt` (12:33, 8.2MB), `oursteps2.txt` (frais, 4 frames),
  `oraclepred2.txt` + `ourpred2.txt` (frais), `oracleistep2.txt` + `ouristep2.txt`, `both.txt`
  (run combiné EC_TRACE_COEFF + EC_TRACE_MODE_STEP, LA référence de session), `mixref.raw`,
  `mixoursnew.yuv`, `mix.y4m`, `mix.obu`, `mix.enc.err`.
- Oracle aomdec : `~/.cache/aom-oracle/build/aomdec`, rung EC_PREDOUT8 pour les pixels prédits.
- L'ancien oraclepred.txt (12:27) est partiel : ne pas s'en servir.

## Fix-now | deferred(<unblock>) | accepted
- `fix-now | deferred(lane suivante) | accepted` : le fix du drift CDF lui-même.
  Unbloqué par : re-diff de l'historique d'adaptation du CDF (les valeurs des TUs 0-18 sont
  archivées dans les traces), puis comparaison de l'ordre d'application des updates CDF vs l'oracle.
