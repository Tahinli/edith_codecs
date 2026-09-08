# lane-arfq (round 2) — a per-LEVEL strength for the objective `delta_q`

Round 1 of this lane swept the pyramid's q ladder (`lanes/arfq.sweep.txt`);
this round is about the per-superblock quantizer lane-dq shipped and the
allocation levers around it. One behaviour change shipped:
`speed::DQ_LEVEL_K = [1.5, 1.0, 1.0]`.

Deciding gate: `encode::tests::bd_rate_film_long_gop` (48 pictures, film rows
only, BD-rate vs libaom `cpu-used 6` / rav1e `speed 6`). Guard gate:
`encode::tests::bd_rate_screen_native` (12 pictures, all five rows). Both
controls were REPRODUCED on this worktree before any arm ran.

## 1. Per-level census, film B, 48 pictures

Ours: `EC_ENC_SS=00:40:00 enc_probe <film B> gate 0 48 150` = **75814 B**,
mean PSNR-Y **45.55 dB** — the long-GOP gate's own q=150 ladder point
(`46.82 dB/75814 B` in the gate table; the gate prints all-plane PSNR, the
census luma-only). Read back by our own decoder through
`EC_AV1_BITCENSUS=1 EC_CENSUS_PERFRAME=1 syntax_census`. rav1e's column is
lane-arfcen/lane-cen3's `speed 6 quantizer 110` point, **75452 B / 45.75 dB**
— within 0.5% of our bytes, so this is a MATCHED-RATE table.

| level | n | q | bytes | share | B/frame | PSNR-Y | rav1e level | n | B/frame | PSNR-Y |
|---|---|---|---|---|---|---|---|---|---|---|
| key | 1 | 102 | 15827 | 21.2% | 15827 | 47.34 | key | 1 | 20763 | 47.95 |
| top ARF (dist 8) | 6 | 118 | 41401 | 55.5% | 6900 | 45.86 | L1 ARF (dist 4) | 11 | 3871 | 45.97 |
| mid ARF (dist 4) | 6 | 142 | 8063 | 10.8% | 1344 | 45.73 | L2 (dist 2) | 12 | 537 | 45.48 |
| leaf | 35 | 162 | 9350 | 12.5% | 267 | 45.42 | leaf | 24 | 178 | 45.69 |

Same shape lane-cen3 found, one merge-worth of encoder improvements later:
our six top ARFs carry 55.5% of the stream at 6900 B each for 45.86 dB where
rav1e's eleven anchors carry 57% at 3871 B for 45.97 dB — **+78% bytes per
anchor frame for −0.11 dB** — and our key is 24% CHEAPER than rav1e's for
−0.61 dB, still the under-quality level.

## 2. The tpl map per level — and why the objective mapping cannot fix this

`EC_AV1_DQ_CENSUS=1` on the same run, aggregated per level (480 superblocks a
frame, `delta_q_res = 4`, the shipped `k = 0.25` at the time of the census):

| level | n | `r_frame` (mean) | coded offsets: frame mean | \|mean\| | nonzero | range |
|---|---|---|---|---|---|---|
| top ARF (q 118) | 6 | **2.914** | −0.32 | 3.72 | 365/480 | −8..12 |
| mid ARF (q 142) | 6 | 1.828 | +0.02 | 2.90 | 295/480 | −4..8 |
| leaf (q 162) | 35 | 1.743 | +0.03 | 2.66 | 273/480 | −8..12 |

The charter's hypothesis was that the top ARF's high `r_frame` (it feeds 16
frames) would already pull its quantizer down. **It does not, and cannot**:
libaom's objective mapping is `beta = (1 + r_sb) / (1 + r_frame)`, i.e. every
superblock is measured against ITS OWN FRAME's ratio, so `r_frame` is the
divisor and cancels. The frame MEAN of the coded offsets is −0.32 qindex at
the top ARF and +0.02/+0.03 at the two lower levels — zero to within a third
of a quantizer step. A `delta_q` strength therefore moves the CONTRAST inside
a frame, never the allocation BETWEEN levels; only a frame q offset
(`Pyramid::arf_q_offset` / `key_q_offset`) moves quality toward a level.
That is the class `frame-blind normaliser`: a per-frame normalised map is
blind, by construction, to the thing the level census is asking about.

## 3. Arms — long-GOP gate (48 pictures)

BD-rate vs libaom `cpu-used 6` / rav1e `speed 6`; lower is better.

| arm | film A | film B | verdict |
|---|---|---|---|
| control (`DQ_K` flat, `k = 0.25` everywhere) | +24.0 / −7.6 | +85.8 / +7.1 | — |
| **(a) top-ARF strength 0.375 (`EC_AV1_DQ_K=1.5:1:1`)** | **+23.5 / −8.0** | **+85.3 / +6.7** | **SHIPPED** |
| (a) top-ARF strength 0.5 (`2:1:1`) | +23.0 / −8.2 | +86.0 / +7.3 | film B up, 12-frame −0.4 |
| (a) top-ARF strength 0.75 (`3:1:1`) | +22.7 / −8.4 | +86.8 / +7.8 | film B up 1.0/0.7 |
| (a) top-ARF strength 1.0 (`4:1:1`) | +21.8 / −8.9 | +88.7 / +9.1 | film B up 2.9/2.0 |
| (b) top ARF at −24 (with 0.5) | +25.8 / −6.8 | +85.5 / +6.8 | film A +1.8 |
| (b) top ARF at −16 (with 0.5) | +28.8 / −5.2 | +90.7 / +9.8 | worse everywhere |
| (c) key at −56 (with 0.5) | +23.8 / −9.4 | +90.1 / +6.4 | libaom +0.8/+4.1 |
| (c) key at −64 (with 0.5) | +24.1 / −10.1 | +93.8 / +6.4 | libaom +1.0/+7.8 |

Control reproduces the charter's numbers (+24.4/−7.5, +86.0/+7.3) to within
0.4 on one column and 0.2 on the others.

**(a) is single-peaked in film B and monotone in film A.** Film A goes on
improving all the way to strength 1.0 (−2.2/−1.3 against control) while film B
turns at 0.375 — 0.375 is the last point down on ALL FOUR columns, which is the
strong form of the keep rule (both films improve on both columns).

**(b) refuted a second time, now WITH the objective mapping on.** The
re-sweep was legitimate — lane-arfq round 1 swept the offsets with no per-SB
`delta_q` at all, so the trade had changed — and it lands where it did:
a shallower top ARF buys film B 0.3 and costs film A 1.8.

**(c) refuted, and it splits the two columns.** A deeper key (−56, −64) is
worth 1.4 / 2.1 BD points on the rav1e column of film A and 0.7 on film B's,
and costs 0.8 / 1.0 and 4.1 / 7.8 on the libaom column: the two references
disagree in SIGN about the key's allocation (class `Opus gate metric split`),
and the keep rule needs both. The key stays at −48.

**The key frame codes no `delta_q` at all**, so the charter's "(a) then the
key" half is inert by construction: the whole lever lives in
`encode_inter_frame`, and a key-frame strength has nothing to scale. That is
why `DqLevel` has three variants and `EC_AV1_DQ_K` three fields.

## 4. Guard gate — 12 pictures, all five rows, at the shipped arm

| clip | control | shipped (`DQ_LEVEL_K = [1.5, 1, 1]`) | Δ |
|---|---|---|---|
| bars 1080p | −3.1 / −18.7 | −3.2 / −18.8 | −0.1 / −0.1 |
| bars 2160p | +9.1 / −13.3 | +9.2 / −13.3 | +0.1 / 0.0 |
| film A | +20.3 / −5.0 | +20.3 / −4.9 | 0.0 / +0.1 |
| film B | +24.6 / −2.0 | +24.7 / −1.8 | +0.1 / +0.2 |
| screen capture | +14.6 / −33.2 | +14.6 / −33.2 | byte-identical |

Nothing worse than +0.2, inside the charter's 0.3 bound. The 0.5 arm was
measured on this gate too and reads film B +25.0/−1.7 (+0.4/+0.3) — over the
bound, which is the second reason 0.375 ships rather than 0.5.

## 5. Shipped

* `speed::DQ_LEVEL_K: [f64; 3] = [1.5, 1.0, 1.0]` — a per-level multiplier on
  `speed::DQ_TPL_K`, indexed by `encode::DqLevel` (top ARF / mid ARF / leaf).
* `encode::DqLevel` rides on `PyramidFrame`; `encode_pyramid_inter` sets it
  from the offset it codes the frame at (the top ARF is the hidden frame at
  the pyramid's own `arf_q_offset`), and the flat non-pyramid path is a leaf.
* `EC_AV1_DQ_K=<top>:<mid>:<leaf>` sweeps it; the `EC_AV1_DQ_CENSUS=1` line
  now names the level it prices.
* Byte pins re-taken: `(150, 8252 → 8269)`, `(60, 33053 → 33044)`, green at
  default and at `EC_AV1_SPEED=6`.
* Self-check: `encode::tests::the_per_level_delta_q_strength_indexes_the_levels_it_names`.

## Deferred

* `deferred: the mid-level and leaf strengths —` only the top ARF was swept
  (it is the level the census names as overspent) and the two others were held
  at the preset's 0.25 in every arm — `unblocked by a lane that can afford
  another six 48-picture arms`.
* `deferred: film A wants 1.0 and film B wants 0.375 —` the strength is
  content-dependent with opposite optima (class `heuristic with two optima`);
  a per-frame decision (e.g. off the frame's own `r_frame` spread) is the
  mechanism that could take both — `unblocked by a lane that charters a
  content-adaptive strength`.
* `deferred: the top ARF's +78% bytes per anchor frame —` the census names it
  and neither of this lane's allocation levers moves it; the mechanism is the
  ARF's own coding cost (lane-arfcen's mode/block-count gap), not its
  quantizer — `unblocked by a lane on the ARF's prediction, not its q`.
