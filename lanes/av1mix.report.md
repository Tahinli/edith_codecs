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

## Diagnostic de la divergence (session r2, 24-09-2026, session Ali-2)

- Le « fork CDF » de r1 (TU#19, cdf0 23340 vs 10886) et le « mi(6,0) br=1 ctx=1 non
  adapté » du r1-bis sont tous deux des **artefacts de convention d'impression** :
  notre `EC_COEFF_STEP tag=all_zero side=` imprime la ligne `txb_skip` **pré**-adaptation
  (copie `dbg_txbskip` avant `dec.symbol`, decode.rs), l'oracle instrumenté imprime
  **post**-adaptation. 25197 = 26876 - (26876>>4) : une adaptation du même défaut neuf
  `AOM_CDF2(5892)` (32768-5892=26876). **Aucune mise à jour CDF manquante.**
- Vrai fork : entre la lecture #151 (chroma) et le txb luma de mi(10,0), nous
  consommons **9 bits** contre **7 chez l'oracle** — un symbole `tx_size_cat1`
  en trop (rng 55700->49918, s=1) lu par `decode_leaf_rect` pour une feuille
  rect **16x8 d'un segment lossless** : le `read_tx_size` de libaom rend TX_4X4
  AVANT le test `TX_MODE_SELECT` (decodeframe.c:1203), donc l'oracle ne code
  aucun symbole de profondeur ici.
- Balayage de la classe : `&& !lossless(fctx)` ajouté aux lectures de profondeur
  de `decode_leaf_rect`, `decode_block_rect`, `decode_block_rect4`,
  `decode_rect4_16_strip`, `decode_block_rect64`, `decode_intra_rect_in_inter`
  (`decode_intra_sub8_leaf`, `decode_leaf_rect8`, le lecteur 128-root et
  `read_tx_size` l'avaient déjà).
- Preuve : mix.obu (cache, non ré-encodé) — chaîne rng EC identique à l'oracle
  (9565 all_zero, 2982 eob, valeurs identiques) et **4 frames byte-exact** vs
  l'aomdec instrumenté (294912 octets, `cmp` implicite par comparaison de
  tableaux). Porte témoin `a_real_aomenc_mixed_lossless_segment_frame_decodes_
  sample_exact` verte (3 arms, sample-exact vs ffmpeg, unités WHT comptées).
- Instrument de session : sonde `EC_SYMR` ajoutée à
  `~/.cache/aom-oracle/src/aom_dsp/bitreader.h` (aom_read_symbol_) — arbre
  scratch hors repo.
