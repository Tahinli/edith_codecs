# lane-dq — the per-superblock `delta_q` mapping, ours vs libaom's objective

## 0. The charter's premise, corrected first

The charter asked what the existing `delta_q` derives its offsets from and
"whether the tpl map feeds it at all". It does, and has since lane-deltaq:
`encode::deltaq_for_factor` inverts the tpl LAMBDA factor through the `ac_q`
table. Nothing was variance-driven and no map was inert. What was never
measured is that OUR mapping and LIBAOM's read the same map into offsets an
order of magnitude apart — that is this lane's finding, and the strength
between them is what shipped.

## 1. Census instrument — `EC_AV1_DQ_CENSUS=1`

One line per inter frame from `encode_inter_frame`, printing, for the SAME
superblocks and the same tpl map, the offsets the shipped mapping
(`deltaq_for_factor`) emits and the ones libaom's objective
(`deltaq_libaom`, new) would, plus which of the two the frame actually coded
(`coded=none` when the lever is off — the census then prices a hypothetical
`res = 4` grid). Zero cost when unset.

Film B, native gate crop `1920x1024` at `00:40:00`, 12 frames, q=150,
preset 0, lever OFF (`enc_probe … gate 0 12 150`, 27634 B = the gate's own
q150 point). 480 superblocks per frame, `base=162`, `res=4`:

| frame | r_frame | ours (range, \|mean\|, nonzero, levels) | libaom (same) |
|---|---|---|---|
| … | 2.70 | −4..8, 3.04, 321/480, 4 | −24..32, 13.97, 450/480, 15 |
| … | 2.85 | −4..8, 2.81, 308/480, 4 | −28..32, 14.94, 454/480, 16 |
| … | 2.83 | −4..8, 2.86, 307/480, 4 | −24..32, 14.32, 448/480, 15 |
| … | 2.51 | −4..8, 2.88, 305/480, 4 | −24..32, 14.45, 451/480, 15 |
| … | 2.06 | −4..8, 2.93, 312/480, 4 | −24..32, 13.84, 449/480, 15 |
| … | 1.74 | −4..8, 2.60, 294/480, 4 | −24..32, 13.58, 447/480, 15 |
| … | 1.27 | −4..8, 2.39, 280/480, 4 | −20..32, 12.03, 448/480, 14 |
| … | 0.91 | −4..4, 1.76, 211/480, 3 | −20..28, 11.07, 450/480, 13 |
| … | 0.45 | −4..4, 1.38, 166/480, 3 | −12..12, 5.87, 409/480, 7 |

**The shipped mapping moves ~5x less quantizer than libaom's on the identical
map**, and it never reaches its own `±16` clamp (its widest offset is `+8`),
so the clamp is NOT what bounds it: the `log2(1+r)` compression plus
`TPL_POWER = 0.5` plus the mean normalisation is. libaom's own clamp
(`±(delta_q_res * 9 − 1)`, `±32` at `res = 4`) IS reached. The census matches
lane-cen3's census fact that libaom codes `delta_q` in every inter frame while
we coded it in none at preset 0.

The census's `libaom` column prints the strength actually in force, so it is
raw libaom (`k = 1`) whenever the lever is off and the shipped `k = 0.25`
otherwise — the shipped default on the same clip prints
`coded=libaom k=0.25 … libaom range −4..4 |mean| 1.21..1.83`, i.e. the
strength brings the objective mapping down to the shipped mapping's own
extent while keeping its SHAPE (different superblocks, 145..220 nonzero vs
166..211), and codes 28304 B against the control's 27634 B.

`aomdec -EC_TRACE_TPL` was not used: it exposes no per-SB qindex, so libaom's
mapping is reimplemented on OUR map instead, which is also the only way to
compare the two mappings at one map (class `shared-oracle blindness` cuts the
other way here — one map, two mappings, is exactly the controlled comparison).

## 2. The mapping, from libaom's source

`av1_get_q_for_deltaq_objective` (`av1/encoder/encodeframe_utils.c:893`) forms
`rk = exp((intra_cost − mc_dep_cost) / w)` in the LOG domain per superblock,
divides the frame-level `r0` (`encoder_utils.c:627`, the same expression at
frame scope) by it, and asks `av1_get_deltaq_offset` (`rd.c`) for the qindex
whose **DC** step is `dc_q(base) / sqrt(beta)`; the offset is clamped to
`±(delta_q_res * 9 − 1)`.

With per-cell costs `intra` and `intra + mc_dep`, `rk = 1/(1 + r)` for this
crate's `r = mc_dep / intra`, so `beta = (1 + r_sb) / (1 + r_frame)` with
`r_frame` read the log-mean way libaom reads `r0`. `encode::deltaq_libaom`
is that; `encode::tpl_frame_ratio` is `r0`. `tpl_lambda_factors` now returns
the raw per-cell ratio beside the lambda factors, because the factor alone has
been through a mean normalisation the ratio cannot be recovered across.

Two things differ from `deltaq_for_factor` and both are the point: the table
inverted is `dc_q` not `ac_q`, and the ratio is read RAW (`sqrt(1+r)`) rather
than through the log-compressed, `TPL_POWER`-flattened, mean-normalised lambda
factor.

Self-check: `encode::tests::the_objective_delta_q_mapping_leans_the_right_way_and_stops_at_libaom_s_clamp`
— direction (leaned-on SB finer, idle coarser, average exactly at base),
`res`-congruence of every offset, libaom's clamp reached from both ends,
monotone in strength, and `tpl_frame_ratio` flat-in/flat-out.

## 3. Sweep — 12-frame `bd_rate_screen_native` (BD vs libaom / rav1e)

Control reproduces the charter to the digit on all three real rows.

| arm | film A | film B | screen |
|---|---|---|---|
| control (no delta_q) | +21.0/−4.8 | +24.9/−1.9 | +14.5/−33.2 |
| shipped mapping (`EC_AV1_DELTAQ=2`) | +20.6/−4.8 | +25.4/−1.1 | byte-identical |
| **objective, k=0.25** | **+20.7/−4.8** | **+24.8/−1.7** | byte-identical |
| objective, k=0.5 | +20.6/−4.5 | +25.8/−0.7 | byte-identical |
| objective, k=1.0 | — | +25.9/−0.1 | — |

At 12 frames the lever is flat at k=0.25 and LOSES at k≥0.5 (film B +0.9/+1.2
at k=0.5, +1.0/+1.8 at k=1.0 — monotone in strength, so k=1.5 was not run;
`deferred` below). The screen capture is byte-identical at every arm: a frame
that set `allow_screen_content_tools` codes no delta syntax at all
(lane-dq3's content gate), so the keep rule's screen bound is met by
construction, not by luck.

## 4. Long GOP — 48 pictures, `bd_rate_film_long_gop` (the shape his exports have)

| arm | film A | film B |
|---|---|---|
| control | +25.2/−7.2 | +86.7/+7.3 |
| shipped mapping | +24.6/−7.3 | +86.1/+7.5 |
| **objective, k=0.25** | **+24.4/−7.5** | **+86.0/+7.3** |
| objective, k=0.5 | +23.8/−7.6 | +86.5/+7.9 |
| objective, k=1.0 | +24.0/−7.0 | +88.4/+9.7 |

Control reproduces the charter to the digit on both rows. **k=0.25 is the only
arm down on both films on the libaom column with neither rav1e column worse**
(film A −0.8/−0.3, film B −0.7/+0.0): it clears the keep rule's second clause
(one row ≥0.5 down, the other flat inside ±0.3) with room, and the screen row
is out of this gate but byte-identical anyway. k=0.5 buys film A another 0.6
and hands film B back 0.6 on the rav1e column; k=1.0 is worse than k=0.5 on
three of the four columns. The lever is a LONG-GOP lever, as lane-cen3
predicted and as the class `short-GOP gate hid the gap` says to expect.

## 5. Shipped

* `speed::DELTAQ_RES[0] = 2` (was 4 = off) — preset 0 codes `delta_q_res = 4`.
* `speed::DQ_TPL_K = [0.25, 0, …]` — the objective mapping at strength 0.25,
  preset 0 ONLY. Presets 5 and 6 keep `deltaq_for_factor` (`DQ_TPL_K = 0`)
  and are byte-identical to before this lane; nothing was measured for them
  and nothing changed.
* `EC_AV1_DQ_TPL=0/1` forces the mapping either way, `EC_AV1_DQ_TPL_K` the
  strength, `EC_AV1_DQ_CENSUS=1` the instrument.
* Byte pins re-taken with a four-line comment: `(150, 8325 → 8419)`,
  `(60, 33014 → 33029)`; green at default and at `EC_AV1_SPEED=6`.

Spec syntax: unchanged from lane-deltaq — the frame's `delta_q_present` /
`delta_q_res` go through `write_delta_q_params`, the per-SB `delta_qindex`
through `tile::write_delta_q` at the spec's `skip → cdef → delta_q` order,
`delta_lf_present` stays 0, and the encoder's own trial decode reads the frame
back (a desync would refuse, class `refusal-from-own-desync`).

## Deferred

* `deferred: objective k=1.5 at 12 frames —` the 12-frame sweep is monotone
  worse in k above 0.25 (film B +0.9 at 0.5, +1.0 at 1.0) and long GOP peaks
  at 0.25/0.5, so 1.5 cannot win either gate — `unblocked by a reason to
  believe the curve turns back up`.
* `deferred: the objective mapping at presets 5 and 6 —` they already ship
  `delta_q` through the OLD mapping and are untouched here; a strength for
  them is a second sweep on a different preset — `unblocked by a lane that
  charters presets 5/6`.
* `deferred: per-level (ARF vs leaf) strength —` lane-cen3 says our top ARF is
  the overspent level; a single frame-type-blind strength cannot move quality
  toward the key, and this lane deliberately did not re-open the pyramid
  offsets — `unblocked by a lane that charters both together`.
