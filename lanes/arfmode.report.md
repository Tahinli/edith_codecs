# lane-arfmode — the top ARF's mode/partition syntax: re-censused, lever formed, gated

Worktree `edith_codecs-arfmode`, branch `lane-arfmode`, base `0d81d6c5`.
Charter: `lanes/arfmode.charter.md` (commit `f4da20b5`). The deferred lever
from the arffilt reconciliation — "the top ARF's mode/partition gap
(+871 B/frame, 2196 blocks vs rav1e's 1507)" — measured at ITS OWN base
first, because five merged encoder waves had moved the stream since
lane-arfpred wrote the number.

## 1. Prior art — reconciled (charter §1)

The residual half of the census was attacked by lane-arfpred/arftf/lookahead
(the temporal filter, strength 2, ±3 window, one-group lookahead) and
lane-tplfut/-tplhalf/-tplfut-arf (the tpl future map at the ARF, re-measured
ON at −1.0/−0.6 film B). `arf_altref` (lane-arfcen arm 2) gives the top ARF
its two distinct past anchors; leaf compound candidates shipped globally in
lanes av1comp3/4 — never A/B'd at the ARF alone. No ARF offer-set knob
existed; `EC_AV1_ARFMODE` is not a second convention beside anything.

## 2. The fresh census at `0d81d6c5`

The gate's own window and crops; ours `enc_probe gate 0 48 150`, rav1e
`speed 6 quantizer 110` cut from the 8-BIT yuv (a librav1e encode fed
straight from the 10-bit mkv produces a yuv420p10le OBU our reader scores at
2 dB against the 8-bit source — the 8-bit path reproduces arfpred's table to
the digit: rav1e top ARF 3872 B/frame, q104, PSNR-Y 45.97).

### 2.1 Family decomposition, B/frame (ours: 6 ARFs @ q118; rav1e: 11 @ q104)

| family | film A ours | film A rav1e | A Δ | film B ours | film B rav1e | B Δ |
|---|---|---|---|---|---|---|
| coeff | 6216 | 4568 | +1647 | 2938 | 1996 | +942 |
| literal | 1928 | 1459 | +469 | 959 | 730 | +229 |
| **mode** | **1845** | **1102** | **+744** | **1137** | **608** | **+530** |
| **mv** | **974** | **520** | **+454** | **719** | **410** | **+310** |
| **partition** | **330** | **191** | **+139** | **256** | **125** | **+131** |
| txsize+txtype | 79 | 4 | +75 | 48 | 2 | +46 |
| **total** | **11374** | **7846** | **+3528** | **6060** | **3872** | **+2188** |

The total gap fell +3120 → +2188 on film B (the merged levers took ~930
B/frame off the top ARF), and its SHAPE flipped: mode+partition+mv is now
44% (film B) / 38% (film A) of the gap — the syntax half the arffilt
reconciliation deferred is the bigger half now.

### 2.2 What the mode family buys (per-table B/frame, census table shares)

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
| segment_id | 0 | 118.4 | 0 | 70.7 |

Three carriers, all ARF-specific: (1) the intra fallback — ours codes
15.0%/4.0% of the ARF's AREA intra (A/B) against rav1e's 12.3%/0.5%, and the
per-block `intra_inter` symbol costs ours 138 B/frame where rav1e pays 10 on
film B: the shared CDF is adapted to a 1-in-5 intra rate instead of 1-in-200;
(2) compound at the ARF — ours 23.8%/18.8% of area, rav1e 0.0% on BOTH
films; (3) an 8x8-heavy partition mix — ours 31.3%/23.3% of blocks are 8x8
against rav1e's 26.3%/13.1%, 1876/2782 blocks a frame against 1507/2272.

## 3. The lever — `EC_AV1_ARFMODE` (default OFF = bit-exact)

`speed::ARFMODE: [u8; 11]` (all 0), `EC_AV1_ARFMODE=<n>` overrides on one
build; bitmask over the TOP ARF's offer set (`DqLevel::TopArf` only — the
mid/quarter hidden frames, the shown leaves and the flat path keep
everything):

| bit | arm |
|---|---|
| 1 | no INTRA candidate at the top ARF — the whole-32 search's intra loop AND the 16x16/8x8 leaf search's intra trial |
| 2 | 16x16 is the leaf floor — the RD-optional 8x8 split is not offered |
| 4 | no compound candidates at the leaves |

The spec-forced 8x8s at straddling 16x16s are a different site and stay;
OFF is bit-exact (pins below).

### The probe's first correction, on the record

The charter scoped arm 1 to the LEAVES and kept the whole-32's intra as "the
scene-cut hatch". The first real-content probe (film A, q150 AND q90) came
back byte-identical to the control: the top ARF's intra AREA lives
entirely in `search_inter_block`'s whole-32 intra loop, and the hatch was a
hatch nothing else was using. Arm 1 was widened to the whole-32 loop
(commit `f1bbf3a0`); the charter's deviation is this paragraph.

## 4. Witnesses (BEFORE gates)

| witness | result |
|---|---|
| `encode::tests::the_encoders_own_streams_are_byte_identical_to_their_pins` | PASS at the default AND `EC_AV1_SPEED=6` — pins `(150, 8291, 0x1f00bb0eb099a27f)`, `(60, 33227, 0x57ee6b1f8eacd881)` hold (OFF = bit-exact) |
| `encoder::tests::the_arf_leaf_offer_set_fires_moves_the_stream_and_decodes_sample_exact` | PASS — at 17 pictures/gop 32, q60, on a three-level luma-ramp fixture (the middle group's top ARF sits between its two anchors, where neither single-ref nor intra wins by default): explicit `ARFMODE=0` byte-identical to the default; modes 1/2/4 each fire their OWN engagement counter (580/113/568 hits) AND move the stream (5241 → 5194/5230/5238 B); coding order and levels unchanged; every armed stream decodes sample-exact through `decode_stream` AND ffmpeg |
| real-content engagement + stream-moves (the two-point probe) | film A arm 1: `arfmode hits: 94088` intra trials skipped at q150 (130584 at q90), stream fnv1a MOVES; arms 2/4 likewise (15407/81432 counts); film B all three arms fire and move — printed per point by `enc_probe`, never assumed |
| `encoder::tests::the_lookahead_holds_one_group_and_still_codes_every_picture` + `the_future_tpl_window_still_codes_every_picture_and_moves_the_stream` | PASS (adjacent-lever interference check: 2 passed) |

(The fixture lesson, for the next lever of this shape: on `noisy_card` alone
NO arm can move anything — 516 intra trials skipped, zero won, stream
identical — because the noise quantizes to skip under both candidates. The
level ramp is what makes intra/compound competitive on synthetic content.)

## 5. Two-point probe (film A/B, q150 + q90, bytes at the control's own PSNR)

| arm | film A | film B | hits/point (A q150 / B q150) |
|---|---|---|---|
| 1 intra-off | −0.28% | **−2.61%** | 94088 / 73248 |
| 2 no-8x8 | **−2.05%** | **−2.49%** | 15407 / 11523 |
| 4 compound-off | −1.58% | −0.30% | 81432 / 63060 |

Nothing dead, nothing promoted on a probe alone (arfpred's rule): the gate
arms are 2, 3 = 1|2, and 7 = 1|2|4 against the control.

## 6. The deciding gates — VPS-2 + VPS-3, `bd_rate_film_long_gop` (48 pictures)

VPS-1 (`2.28.112.3`) is unreachable from this host. Re-ran 2026-09-14 on
VPS-2 (`tCloud@2.28.124.204`: control, arm 2, arm 3) and VPS-3
(`tCloud@178.105.165.182`: control, arm 7). Same recipe: repo-arfmode,
target-arfmode, fixtures symlink, `rm -rf .git && git init -q`, PATH
`$HOME/.cargo/bin:/home/tCloud/gates/bin:/usr/bin` inside the unit, chain
as a user unit, linger on. Film hashes
`2112cf3753b4…` / `79e1800804d5…`. Each VPS ran its own control so arm
deltas stay on that substrate.

Each log row is `film | ladder points | BD vs libaom | BD vs rav1e | ours:libaom:rav1e walls`.

| gate | film A libaom / rav1e | film B libaom / rav1e | wall A / B (ours) | loadavg before → after |
|---|---|---|---|---|
| VPS-2 control | **+20.9% / −9.0%** | **+70.4% / −2.2%** | 1422.5s / 1109.6s | 1.85 → 1.03 |
| VPS-2 arm 2 | +21.8% / −8.6% | +71.0% / −1.9% | 1316.6s / 1031.7s | 1.03 → 1.13 |
| VPS-2 arm 3 = 1\|2 | +23.8% / −7.8% | +70.1% / −3.1% | 1298.4s / 1033.8s | 1.13 → 1.18 |
| VPS-3 control | **+20.9% / −9.0%** | **+70.4% / −2.2%** | 1188.3s / 932.1s | 1.47 → 1.16 |
| VPS-3 arm 7 = 1\|2\|4 | +25.8% / −6.7% | +72.1% / −2.1% | 1037.8s / 818.1s | 1.16 → 1.03 |

Both controls reprint the standing table to the digit (film A +20.9/−9.0,
film B +70.4/−2.2), same byte ladders as each other
(A 186167/319027/531588/1120986, B 68680/138142/288789/784645). Arm
deltas are therefore attributable.

Delta vs same-VPS control (libaom column = merge currency):

| arm | film A | film B | rav1e A / B | wall |
|---|---|---|---|---|
| 2 no-8x8 | **+0.9 worse** | **+0.6 worse** | +0.4 / +0.3 | −7% / −7% |
| 3 = 1\|2 | **+2.9 worse** | −0.3 | +1.2 / −0.9 | −9% / −7% |
| 7 = 1\|2\|4 | **+4.9 worse** | **+1.7 worse** | +2.3 / +0.1 | −13% / −12% |

Keep rule: one film ≥0.5 BD down, the other flat ±0.3, wall ≤ +15%.
Walls all pass (faster). Quality does not: no arm has a film at ≥0.5
down with the other flat. Arm 3's film B rav1e −0.9 is the only ≥0.5
column, and film A is not flat. Bytes dropped; PSNR dropped more.
The two-point probe's sign was wrong (arm 2 −2.05%/−2.49% bytes at the
control's PSNR) — same class as lane-arfpred (`probe-sign-mismatch`).

Chain walls: VPS-2 lgc 52m14s, lga2 49m16s, lga3 49m21s; VPS-3 lgc
44m01s, lga7 39m39s. Warm 53s / 45s.

## 7. Guard gate — `bd_rate_screen_native` (12 pictures, five rows)

Skipped. The native pair is required for an ON disposition; keep-rule
failed on the long-GOP table, so there is no disposition arm to guard.
accepted — not deferred.

## 8. Keep rule and disposition

**Default OFF — keep-rule not met.** `speed::ARFMODE` stays `[0; 11]`,
pins `(150, 8291)` / `(60, 33227)` hold, `EC_AV1_ARFMODE=<n>` remains the
A/B override. Cutting the top ARF's intra / 8x8 / compound offer set
saves syntax bytes and loses more in residual. The probe is not a
promotion signal for this lever.


## 9. What this lane did NOT do

* `deferred: the compound trio at the ARF alone (arm 4): the probe reads
  film A −1.58% / film B −0.30%. Arm 7 (1|2|4) was the worst long-GOP
  arm on film A (+4.9 vs libaom) — do not promote on the probe.`
* `deferred: motion_mode/warp syntax at the ARF (64-90 B/frame, rav1e 0) —
  untouched; warp is a shipped default and an ARF-only cut is a fresh
  measurement.`
* `deferred: segmentation (rav1e pays 70-118 B/frame of segment_id at its
  ARF to buy cheap skip) — ours codes no segments at all; a whole lane.`
* `deferred: NEW-mv share at the ARF (ours ~45% vs rav1e ~26%) — driven by
  the lag-8 anchor pair; the mv column (+310/+454) is block count plus
  longer residuals, not a syntax lever.`

Lever already merged default-OFF at `dcbddc65`. This update is the keep-rule close.
