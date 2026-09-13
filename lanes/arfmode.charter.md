# lane-arfmode — the top ARF's mode/partition syntax

Worktree `edith_codecs-arfmode`, branch `lane-arfmode`, base `0d81d6c5`. The
lane the arffilt reconciliation defers: "the top ARF's mode/partition gap
(+871 B/frame, 2196 blocks vs rav1e's 1507) — the residual half of the census
is done, the syntax half is not". The +871 was written at lane-arfpred's base
`35874aef`, before the temporal filter, the tpl future map, `arf_altref` and
the 128 roots shipped; this lane re-measures the gap at its own base, forms
the lever from what is LEFT of it, and takes it to the deciding gate.

## 1. Prior art — reconciled, nothing rebuilt

| prior lane | what it established at the top ARF | state at this base |
|---|---|---|
| lane-arfcen | the per-level stream census; the gap sits in the six pictures coded at distance 8 | instrument shipped (`EC_CENSUS_PERFRAME=1`) |
| lane-arfpred | `arf_pred_census` (lag is NOT the cause); the +871 B mode/partition decomposition; `arf_temporal_filter` shipped at strength 3 | residual half attacked; syntax half deferred to THIS lane |
| lane-arftf | filter strength 3 -> 2, bracketed; mid-ARF filter refuted | shipped (`EC_AV1_ARF_TF`) |
| lane-lookahead | one-group lookahead; symmetric +-3 filter window | shipped (`EC_AV1_LOOKAHEAD`, `ARF_TF_WIN_FUT`) |
| lane-tplfut / lane-tplhalf / lane-tplfut-arf | the tpl future map at the ARF — shipped, re-measured ON (film B -1.0/-0.6); the arffilt pointer to it was stale | shipped (`TPL_FUT=2`, `HALF=2`, `WIN=11`) |
| lane-arfq / lane-keyq | ARF q offsets and key allocation swept | no mode/partition lever |
| lane-arfcen arm 2 (`arf_altref`) | the top ARF's second, distinct past anchor — what its compound candidates read | shipped |
| lanes av1comp3/av1comp4 | leaf compound candidates (5 shapes) shipped GLOBALLY — never A/B'd at the ARF alone | open at ARF granularity |
| lanes census-filmB / census-longgop | level allocation; leaves already cheap at 48 pictures | context |

No knob named `EC_AV1_ARFMODE` or similar exists; nothing here is a second
convention beside a shipped one. The census instrument
(`examples/syntax_census`, `EC_AV1_BITCENSUS=1 EC_CENSUS_PERFRAME=1`) is the
charter's reader — ours and rav1e measured by the same decoder.

## 2. The fresh census at `0d81d6c5` — where the gap is NOW

Recipe: the long-GOP gate's own window and crops, ours `enc_probe gate 0 48
150`, rav1e `speed 6 quantizer 110` from the 8-bit yuv (a 10-bit rav1e stream
does not decode in our reader — PSNR reads 2 dB; the 8-bit re-cut reproduces
arfpred's table to the digit: rav1e top ARF 3872 B/frame, PSNR-Y 45.97,
base_q_idx 104). `EC_CENSUS_TABLES=24`.

### 2.1 Family decomposition, B/frame, ours (q118, 6 ARFs) vs rav1e (q104, 11 ARFs)

| family | film A ours | film A rav1e | A delta | film B ours | film B rav1e | B delta |
|---|---|---|---|---|---|---|
| coeff | 6216 | 4568 | +1647 | 2938 | 1996 | +942 |
| literal | 1928 | 1459 | +469 | 959 | 730 | +229 |
| **mode** | **1845** | **1102** | **+744** | **1137** | **608** | **+530** |
| **mv** | **974** | **520** | **+454** | **719** | **410** | **+310** |
| **partition** | **330** | **191** | **+139** | **256** | **125** | **+131** |
| txsize+txtype | 79 | 4 | +75 | 48 | 2 | +46 |
| **total** | **11374** | **7846** | **+3528** | **6060** | **3872** | **+2188** |

The total gap is down from +3120 to +2188 on film B (the merged levers took
~930 B/frame off the top ARF), and its SHAPE flipped: mode+partition+mv
(+971 on film B, +1337 on film A) is now 44%/38% of the gap and the single
biggest mode-family carrier is syntax this encoder OFFERS and rav1e does not
spend at its ARF.

### 2.2 What the mode family buys (per-table, B/frame, from the census's own table shares)

| table | A ours | A rav1e | B ours | B rav1e |
|---|---|---|---|---|
| skip | 228.0 | 226.4 | 184.8 | 132.8 |
| intra_inter | 194.5 | 76.3 | 138.3 | 10.2 |
| comp_mode | 193.0 | 0 | 119.7 | 0 |
| y_mode | 184.9 | 58.4 | 78.2 | 2.5 |
| inter_compound_mode | 162.7 | 0 | 66.9 | 0 |
| uv_mode_cfl | 205.5 | ~0 | 24.7 | 0 |
| new_mv | 157.2 | 201.7 | 145.6 | 143.5 |
| motion_mode | 90.4 | 0 | 63.9 | 0 |
| drl_mode | — | — | 58.3 | 7.0 |
| ref_mv | — | 171.4 | 31.2 | 120.1 |
| segment_id | 0 | 118.4 | 0 | 70.7 |

Three carriers, all ARF-specific:

1. **The intra fallback poisons shared CDFs.** Ours codes 4.0% (film B) /
   15.0% (film A) of the ARF's AREA intra where rav1e codes 0.5% / 12.3%;
   the per-block `intra_inter` symbol then costs ours 138 B/frame on film B
   where rav1e pays 10 — the CDF is adapted to a 1-in-5 intra rate instead
   of 1-in-200. The intra blocks' own mode syntax (y_mode, uv_mode_cfl,
   filter_intra) rides on top. CAUTION: on film A even rav1e keeps 12.3%
   intra area — there the intra fallback is CONTENT (grain), not waste.
2. **Compound at the ARF: ours 18.8%/23.8% of area, rav1e 0.0% on both
   films.** The trio (comp_mode + inter_compound_mode + comp_ref_type) costs
   ~200 B/frame (film B) to ~360 (film A). Never A/B'd at the ARF alone.
3. **The partition mix is 8x8-heavy**: ours 23.3%/31.3% of blocks are 8x8
   against rav1e's 13.1%/26.3%, and ours codes 1876/2782 blocks a frame
   against 1507/2272 — every extra leaf pays its own skip + intra_inter +
   ref + mode + mv symbols. Ours takes the 8x8 split at 2.4x rav1e's rate on
   film B.

NEWMV share of single-ref modes: ours ~45% both films, rav1e ~25% — the
+310/+454 mv column is block count plus longer mv residuals.

## 3. The lever — `EC_AV1_ARFMODE` (default OFF = bit-exact)

A per-frame offer-set change, TOP ARF ONLY (`DqLevel::TopArf` — the group's
own anchor, `arf_q_offset`), leaves only; the 32x32 whole keeps its full
offer set as the scene-cut hatch, and spec-forced 8x8s at straddling edges
are untouched. Bitmask, values OR-able:

| value | arm | census target |
|---|---|---|
| 1 | ARF leaves (16x16/8x8) offer NO intra candidate: the leaf search runs its inter set only | intra_inter + y_mode + uv_mode_cfl + filter_intra, and the CDF de-poisoning across every block |
| 2 | 16x16 is the ARF's leaf floor: the RD-optional 8x8 split is not offered | partition + the three extra leaves' mode/mv syntax per split |
| 4 | ARF leaves offer NO compound candidates | comp_mode + inter_compound_mode + comp_ref_type (~200-360 B/frame) |
| 3 / 5 / 6 / 7 | combinations | — |

`speed::ARFMODE: [u8; 11]` ships the gate's disposition per preset;
`EC_AV1_ARFMODE=<n>` overrides for A/B on one build. OFF (0, the default)
must be byte-exact with the pre-lane encoder — pins `(150, 8291)`,
`(60, 33227)` hold at the default and at `EC_AV1_SPEED=6`.

## 4. Witnesses BEFORE gates

* Pins at the default AND `EC_AV1_SPEED=6` (OFF bit-exactness).
* Engagement counters (class `gate-blind-to-feature`): `ARFMODE_HITS` —
  [0] ARF leaves coded with the intra trial skipped, [1] ARF 8x8 offers
  refused, [2] ARF leaves with compound not offered — asserted > 0 on a real
  film encode, and the armed stream must MOVE.
* Every witness stream decodes sample-exact through `decode_stream` AND
  ffmpeg at the usual picture counts.

## 5. Measurement ladder

1. Local two-point probe (film A and film B, q150 + q90, bytes at equal PSNR)
   prunes dead arms before any VPS time (arfpred's rule: the probe is a weak
   proxy — it PRUNES, it does not PROMOTE; surviving arms go to the gate).
2. VPS-1 (`tCloud@2.28.112.3`), managed skill `ec-av1-vps-gate-runners`
   (repo-arfmode, target-arfmode, fixtures symlink, `.git` stub, PATH shim
   inside the unit, chain as a user unit, linger on): `bd_rate_film_long_gop`
   control first — must reproduce the standing table (film A +20.9/-9.0,
   film B +70.4/-2.2) +-0.2/cell — then the surviving arms, one gate at a
   time, LOADAVG before/after each.
3. `bd_rate_screen_native` (five rows) for the disposition arm — at 12 frames
   `group_target` forms ONE group of 11 whose ARF IS a `TopArf`, so the
   native gate ENGAGES this lever (no blindness claim; the arm runs).

## 6. Keep rule and disposition

One film row >=0.5 BD down on a column, the other film flat (±0.3), screen
not worse 0.3, wall <=+15%. Met -> default ON at the measured presets, pins
re-taken at the default and `EC_AV1_SPEED=6`, three-lane lib suite (fixtures
symlinked), `cargo check --workspace --all-targets -j4`. Not met -> default
OFF / revert, document.

## 7. Risks

* Arm 1 on film A: 15% intra area would flip to inter where even rav1e keeps
  12% — the residual cost may eat the syntax win on the grain film exactly.
* Arm 2: the 8x8 splits it refuses won RD; the syntax win (~280 B/frame
  film B by the block arithmetic) has to beat the residual regression.
* Arm 4: the two-ref prediction win at the ARF is real (arf_altref exists
  for it); only the syntax half is measured.
* The probe has mispredicted gate signs before (arfpred); nothing ships off
  a probe alone.
