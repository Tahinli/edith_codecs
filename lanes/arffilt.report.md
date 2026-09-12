# lane-arffilt — RECONCILED: the ARF temporal filter already shipped; the charter's experiment is its own instrument

Worktree `edith_codecs-arffilt`, branch `lane-arffilt` off main `7db5094a`.
**No encoder change was made and none was needed**: charter step 1 (check
prior art, reconcile rather than rebuild) resolved the whole charter. Every
piece it asks for — the lag/SAD experiment, the `exp(-mse/strength)`
motion-compensated ARF source filter, the witnesses, the deciding-gate
evidence — merged before this base, in three lanes whose merge commits are
all ancestors of `7db5094a`:

| prior lane | merged | what it shipped |
|---|---|---|
| lane-arfcen | (census) | the stream census the charter cites: top ARF 6992-8021 B/frame vs rav1e's 3872 at coarser q (118 vs 104), 61% of the gap residual |
| lane-arfpred | `8d9d056b` | `encode::tests::arf_pred_census` (the charter's experiment, verbatim) + `encode::arf_temporal_filter` + `speed::ARF_TF`, ships strength 3, `ARF_TF_QMIN=16` |
| lane-arftf | `aabb73eb` | strength swept ON the gate: 3 → **2** (bracketed by 1 and 5); 4-past window and mid-ARF filter refuted |
| lane-lookahead | `0dc2f731` | one mini-GOP of lookahead (`speed::LOOKAHEAD`), symmetric **±3** filter window (`ARF_TF_WIN=ARF_TF_WIN_FUT=3`), bracketed ±0..±4 |

The charter's evidence comment (`encode.rs`, the 6992-vs-3872 sketch) is not
a proposal — it is `arf_pred_census`'s own doc-comment, written by
lane-arfpred to describe the instrument it had already run.

## 1. The SAD experiment, re-run at this base — reproduces to the digit

`encode::tests::arf_pred_census` at `7db5094a` (3-4 s per arm, no encoder
involvement; the same log-step diamond against lag 8, lag 4, and the
temporally filtered block against lag 8):

| arm | clip | lag8 SAD/px | lag4 SAD/px | tf lag8 SAD/px | tf removes | tf-vs-source PSNR |
|---|---|---|---|---|---|---|
| s30, ±2 both sides | film A | 2.484 | 2.369 | 2.315 | **6.8%** | 47.74 dB |
| s30, ±2 both sides | film B | 1.199 | 1.114 | 1.028 | **14.3%** | 49.54 dB |
| s30, 2 past (arfpred's table) | film A | 2.484 | 2.369 | 2.337 | 5.9% | 48.30 dB |
| s30, 2 past (arfpred's table) | film B | 1.199 | 1.114 | 1.059 | 11.7% | 50.14 dB |
| s2 (the shipped strength), ±2 | film A | 2.484 | 2.369 | 2.391 | 3.8% | 53.09 dB |
| s2 (the shipped strength), ±2 | film B | 1.199 | 1.114 | 1.077 | 10.2% | 52.71 dB |

The first four rows match `lanes/arfpred.report.md` §2 **to the digit** on
this base, four merge-waves later — the instrument and its numbers are
stable, and the lag reading stands: halving the anchor's prediction distance
is worth 4.6-7.1% of prediction SAD while the byte gap is 81%; the residual
is mostly source grain.

## 2. Disposition — the charter's <15% stop-rule vs the gate evidence

Read alone, the stop rule fires: the best census number (14.3%) sits under
the 15% bar. The merged prior art resolves this exact tension, and in the
lever's favour — **the SAD census is a weak proxy for this lever's product
effect** (lane-arfpred's own two-point probe mispredicted the SIGN of the
gate result; the byte savings at high strength come back as leaf bits).
What the deciding gate actually measured, across the three lanes:

| arm (long-GOP gate, BD vs libaom cpu-used 6 / rav1e speed 6) | film A | film B |
|---|---|---|
| no filter (lane-arfpred control, RUN not quoted) | +23.5 / −8.0 | +85.3 / +6.7 |
| `ARF_TF=3`, 2 past (lane-arfpred ships) | +22.7 / −7.9 | +79.0 / +2.7 |
| `ARF_TF=2`, 2 past (lane-arftf ships) | +22.2 / −8.2 | +78.6 / +2.7 |
| `ARF_TF=2`, ±3 (lane-lookahead ships; this base) | +21.2 / −8.8 | +72.1 / −1.2 |

Guard gate `bd_rate_screen_native` at every step: the two bars rows move ≤0.3,
film rows improve or hold, screen capture byte-identical (its sequence codes
no pyramid ARF — the filter never runs there). The current standing table
(film A +20.9/−9.0, film B +70.4/−2.2) is measured WITH the filter on.

**Disposition: the lever is alive in the only currency that counts (the
deciding gate), and it is already shipped ON by default at every preset.**
The charter's keep rule (one film row ≥0.5 down, other flat, screen not worse
0.3, wall ≤+15%) was met on lane-arfpred (film B −6.3 libaom column, film A
−0.8, screen within bounds, and the filter FASTER: 167 s → 147 s on the
film-B two-point probe), then re-met by arftf and lookahead (down on all
four columns each, bracketed). Step 6's "default ON + pins + lib suite +
cargo check" therefore already happened on those lanes (suites 585/587/590
passed, `cargo check --workspace --all-targets` clean, pins re-taken at each
step). Nothing here is rebuilt: **creating a second, default-OFF
`EC_AV1_ARFFILT` knob beside the shipped `EC_AV1_ARF_TF` would be a second
convention beside an existing one, and is refused.** The charter's OFF maps
to `EC_AV1_ARF_TF=0` (and `EC_AV1_LOOKAHEAD=0` for the future half's buffer).

## 3. The shipped state at this base (verified, not assumed)

* `speed::ARF_TF = [2.0; 11]`, override `EC_AV1_ARF_TF=<strength>`;
  `ARF_TF_WIN = ARF_TF_WIN_FUT = [3; 11]` (±3, override `EC_AV1_ARF_TF_WIN`
  / `_WIN_FUT`); `ARF_TF_MID = [0.0; 11]` (refuted, ships off);
  `ARF_TF_QMIN = 16` (off at the rate loop's quantizer floor, libaom's rule).
* `encoder.rs` (`code_group`): the top ARF's padded source is replaced by
  `arf_temporal_filter(&source, [3 past | 3 future], strength)` whenever
  strength > 0 and `base_q_idx >= 16` — per 16x16 luma block,
  motion-compensated (log-step diamond), weight `exp(-mse/strength)`, chroma
  on the halved vector with its own weight sum. The charter's
  "motion-compensated is the follow-up arm" is not pending: the shipped
  filter IS motion-compensated; only plain translational averaging would be
  a regression.
* The bitstream is spec-legal by construction (the ARF is just a frame whose
  input is filtered; no syntax change) — and the decoder never filters, so
  exactness is encoder-recon-path only, which the witnesses cover.

## 4. Witnesses, re-proven on THIS worktree at `7db5094a`

| witness | result |
|---|---|
| `encode::tests::the_encoders_own_streams_are_byte_identical_to_their_pins` | PASS — pins `(150, 8291, 0x1f00bb0eb099a27f)`, `(60, 33227, 0x57ee6b1f8eacd881)` hold |
| engagement probe: same test with `EC_AV1_ARF_TF=0` | FAILS AS DESIGNED — stream moves to `(150, 8229, 0x5dc0…)`: the filter genuinely engages at the default and OFF is a different, real stream (counter-substitute; no numeric TF counter ships — the pins and the stream-moves guard are the engagement guard) |
| `encoder::tests::the_lookahead_holds_one_group_and_still_codes_every_picture` | PASS — byte-identical with future empty, stream MOVES with a future neighbour (gate-blind-to-feature guard), sample-exact through `decode_stream` AND ffmpeg at 1/7/8/9/17 pictures |
| `encoder::tests::every_speed_preset_decodes_sample_exact_through_both_decoders` (`--ignored`) | PASS (16.6 s) — every preset's stream decodes sample-exact through ffmpeg and `decode_stream`, i.e. the filtered-source ARF reconstructs identically |

## 5. Gates, walls, LOADAVG

No BD gate was re-run in this lane. The arm-vs-control contrast the charter
asks for was measured on the deciding gate itself by the three merged lanes
(tables above, controls RUN not quoted, one gate at a time, LOADAVG pairs in
their reports); re-running it would re-measure merged history on a base that
has not moved. Local runs this lane: census 2×4 s, engagement probe 5 s,
witnesses 20 s + 17 s, sequential, no sibling-visible load.

## 6. What this lane did NOT do (the open levers the prior reports name)

* `deferred: the top ARF's mode/partition gap (+871 B/frame, 2196 blocks vs
  rav1e's 1507) — the residual half of the census is done, the syntax half
  is not — unblocked by a partition lane at the anchor (or the parked
  128-root work).`
* `deferred: per-block weight refinement in arf_temporal_filter (libaom
  weights each 16x16 block against each neighbour separately, up to 7
  frames) — untouched since lane-arftf, env knobs make the arm cheap.`
* `deferred: the tpl future map at the ARF (the buffered future pictures
  reach the source filter only, not the temporal lambda map) — now a
  one-argument change per lane-lookahead §5.`
* `deferred: two-pass-like allocation over the buffered group — the buffer
  the lookahead lane added exists and is unused by the rate loop.`

Pins: `(150, 8291)`, `(60, 33227)` — confirmed by test on this worktree.
No merge, no push.
