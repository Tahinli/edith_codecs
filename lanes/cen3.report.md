# lane-cen3 — where film B's bytes go at MATCHED RATE, ours vs libaom vs rav1e

Measurement lane: no encoder behaviour changed. One instrument added
(`EC_CENSUS_TABLES`, below). Every number is read by OUR decoder through
`EC_AV1_BITCENSUS=1 EC_CENSUS_PERFRAME=1 examples/syntax_census`, so all three
encoders are measured by the same reader; PSNR-Y is computed in that tool
against the very pixels the encoders were handed.

Window: the native gate's own film B crop — `crop=1920:1024:960:292` at seek
`00:40:00` of the 2160p source, 8-bit 4:2:0, one tile, one thread, `gop =
frames`. Ours is `enc_probe <clip> gate 0 <frames> 150` (byte-exact with the
gate's `q=150` ladder point: 12 frames 27634 B, 48 frames 71601 B).
References are the gate's own ffmpeg recipes at a finer crf/quantizer grid, so
the comparison can be made at MATCHED BYTES rather than at the ladder's four
points. Artifacts: `$HOME/.cache/cen3/{12f,48f}` (streams, `cen-*.txt`
censuses, `parse.py`/`agg.py`/`perblock.py`/`modes.py`).

## 0. The matched-rate points

| window | stream | bytes | PSNR-Y | vs ours |
|---|---|---|---|---|
| 12 frames | **ours q150** | **27634** | **45.491** | — |
| 12 frames | libaom `cpu-used 6` crf 36 | 26923 | 46.033 | −2.6% bytes, **+0.54 dB** |
| 12 frames | rav1e `speed 6` q125 | 26688 | 45.563 | −3.4% bytes, +0.07 dB |
| 48 frames | **ours q150** | **71601** | **45.444** | — |
| 48 frames | libaom `cpu-used 6` crf 34 | 70345 | 46.539 | −1.8% bytes, **+1.10 dB** |
| 48 frames | rav1e `speed 6` q110 | 75452 | 45.751 | +5.4% bytes, +0.31 dB |

Local slope of our own 48-frame ladder (q150 71601 B/45.444, q120 149971 B/
46.730): **1.21 dB per doubling**, i.e. +10% bytes = +0.166 dB. Every "bytes
→ dB" conversion below uses that number. The 1.10 dB deficit against libaom
over 48 pictures is therefore worth **+87% bytes at matched PSNR**, which
reproduces `libcen`'s +92% on the same window at the same recipe (our streams
have since got smaller: 90674 B → 71601 B at q150).

## 1. Where the bytes go — whole stream, 48 frames, matched rate

Family bytes (census, ±0.03% of payload on every stream):

| family | ours | libaom crf34 | rav1e q110 | ours − libaom |
|---|---|---|---|---|
| coeff | 33016 | 36627 | 37460 | **−3611** |
| literal (raw coeff bits) | 10917 | 14231 | 13400 | **−3314** |
| **mode** | **15770** | **6592** | 12729 | **+9178** |
| mv | 7175 | 8720 | 8068 | **−1545** |
| partition | 2890 | 1807 | 2272 | +1083 |
| txsize | 322 | 127 | 0 | +195 |
| txtype | 255 | 995 | 67 | −740 |

Read it in one line: **at the same total size we spend 9.2 kB more on mode
syntax and 6.9 kB less on actual residual than libaom does, and we are 1.10 dB
worse.** Signalling is eating the residual budget. rav1e sits between us.

Split by frame type (the key is ONE frame in all three streams):

| | ours | libaom | rav1e |
|---|---|---|---|
| key bytes / PSNR-Y / qindex | 15827 / **47.34** / 102 | 28803 / 47.05 / 41 | 20763 / **47.95** / 80 |
| inter bytes (47 frames) | 55774 | 41542 | 54689 |
| inter mean PSNR-Y | 45.40 | **46.53** | 45.70 |

Our key frame is the cheapest of the three and beats libaom's by 0.29 dB at
55% of its bytes (`libcen` found the same on both films). **The entire gap is
inter: +34% bytes for −1.13 dB against libaom.** Inter families, ours −
libaom: mode **+8804 B**, coeff +5633 B, partition +1125 B, literal +627 B,
mv **−1545 B**.

## 2. Per level, 48 frames, matched rate

Ours (`8:-32:12:-8:-48` pyramid), by qindex:

| level | n | q | bytes | share | B/frame | PSNR-Y | skip% | comp% | blocks/frame | mode b/blk | mv b/blk | coeff b/blk |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| top ARF (dist 8) | 6 | 118 | 38930 | 55.3% | 6488 | 45.75 | 59.9 | 22.3 | 2123 | 4.77 | 2.17 | 12.27 |
| key | 1 | 102 | 15827 | 22.5% | 15827 | 47.34 | 0.0 | 0.0 | 1479 | 5.66 | 0.00 | 57.69 |
| leaf | 35 | 162 | 8304 | 11.8% | 237 | 45.31 | 98.1 | 57.4 | 543 | 1.94 | 0.83 | 0.34 |
| mid ARF (dist 4) | 6 | 142 | 7377 | 10.5% | 1229 | 45.63 | 89.3 | 58.4 | 917 | 3.65 | 2.52 | 2.89 |

libaom `cpu-used 6` crf 34 (top rows; 17 distinct qindex levels in all):

| level | n | q | bytes | share | B/frame | PSNR-Y | skip% | blocks/frame | mode b/blk | mv b/blk | coeff b/blk |
|---|---|---|---|---|---|---|---|---|---|---|---|
| key | 1 | 41 | 28803 | 41.6% | 28803 | 47.05 | 0.0 | 1289 | 4.17 | 0.00 | 123.6 |
| ALTREF (hint ≈30) | 1 | 52 | 16885 | 24.4% | 16885 | **48.56** | 60.2 | 1529 | 4.22 | 4.91 | 55.2 |
| 2nd anchor | 1 | 104 | 5567 | 8.0% | 5567 | 47.38 | 72.8 | 996 | 3.88 | 7.54 | 22.0 |
| mid | 4 | 128 | 3113 | 4.5% | 778 | 46.14 | 90.7 | 478 | 3.27 | 4.47 | 2.84 |
| leaves | 22 | 136 | 2737 | 4.0% | **124** | **46.33** | 99.8 | **156** | 2.25 | 3.20 | 0.32 |

rav1e `speed 6` q110:

| level | n | q | bytes | share | B/frame | PSNR-Y | skip% | blocks/frame | mode b/blk | mv b/blk | coeff b/blk |
|---|---|---|---|---|---|---|---|---|---|---|---|
| key | 1 | 80 | 20763 | 28.0% | 20763 | 47.95 | 0.0 | 1266 | 5.23 | 0.00 | 92.6 |
| L1 ARF (dist 4) | 11 | 104 | 42590 | 57.5% | 3871 | 45.97 | 80.7 | 1507 | 3.23 | 2.17 | 10.59 |
| L2 (dist 2) | 12 | 122 | 6446 | 8.7% | 537 | 45.48 | 98.7 | 732 | 2.49 | 2.03 | 0.65 |
| leaves | 24 | 139 | 4290 | 5.8% | 178 | 45.69 | 99.8 | 528 | 1.57 | 0.84 | 0.09 |

## 3. Ranked table — where our bytes go that they do not spend

Bytes at the 48-frame matched point (71601 B ours). "dB equivalent" uses the
1.21 dB/doubling slope.

| # | what | ours | libaom | gap | % of our stream | dB equiv |
|---|---|---|---|---|---|---|
| 1 | **per-block syntax (mode+partition+txsize)** driven by BLOCK COUNT: 38754 coded blocks vs 16178 | 18982 B | 8526 B | **+10456 B** | 14.6% | **0.24** |
| 2 | **inter coefficients** (excess residual left by weaker prediction) | 22350 B | 16717 B | +5633 B | 7.9% | 0.13 |
| 3 | partition alone (inside #1) | 2890 B | 1807 B | +1083 B | 1.5% | 0.03 |
| 4 | motion vectors — we spend LESS | 7175 B | 8720 B | **−1545 B** | −2.2% | — |
| 5 | tx_type — we spend LESS (set3/set6 only) | 255 B | 995 B | −740 B | −1.0% | — |
| 6 | `delta_q` (per-SB, libaom every inter frame; we code none) | 0 B | 254 B | −254 B | −0.4% | — |

Where their PSNR is higher for the same bytes, by level: libaom's LEAVES cost
124 B/frame at 46.33 dB where ours cost 237 B/frame at 45.31 dB (**half the
bytes, +1.02 dB**), and libaom's whole 48-picture window never drops below
45.08 dB while ours falls monotonically from 47.34 to 45.31 (2.03 dB spread
against libaom's 47.05 → 46.33 = 0.72 dB).

## 4. One level down on the top three families

### 4a. mode — it is block COUNT, not per-block cost

| | ours | libaom | rav1e |
|---|---|---|---|
| coded blocks (48 frames) | 38754 | **16178** | 39303 |
| mode bits per block | **3.26** | **3.26** | 2.59 |
| block sizes | 64x64 53.0%, 32x32 20.7%, 16x16 17.6%, 8x8 8.7%, **no 128** | **128x128 28.6%**, 32x32 28.1%, 64x64 19.5%, 16x16 16.9%, 8x8 1.8% | 64x64 52.0%, 32x32 20.9%, 16x16 20.2%, 8x8 6.9% |
| leaf-level blocks/frame | 543 | **156** | 528 |
| leaf blocks at 64x64+ | 85.5% | 89.0% | 88.8% |

Our per-block mode cost is IDENTICAL to libaom's to two decimals — the 9.2 kB
mode gap is 2.4x the block count and nothing else. At the leaf level, where
the ratio is worst (543 vs 156 blocks a frame), **85.5% of our blocks are
already 64x64, the largest root our sequence header allows**
(`use_128x128_superblock = 0`): a 1920x1024 frame is 480 64-roots for us and
120 128-roots for libaom, and both encoders mostly code one block per root.
The only reachable mechanism for this gap is the 128 root. Top tables of our
leaf level confirm it is pure signalling: `mv_comp 18.3%  comp_mode 14.7%
inter_compound_mode 10.6%  new_mv 7.9%  ref_mv 7.4%  mv_joint 5.8%  skip 5.5%
partition_w64 4.8%` — coefficients are 1.3% of a leaf's bits.

### 4b. coeff — prediction, not quantisation

Our inter coefficient excess (+5633 B) sits at the ARF level: 6 frames × 6488 B
with 59.9% skip area and 12.27 coeff bits/block. libaom holds **72.8% skip at
a reference distance of ~30 pictures** where ours is 59.9% at distance 8, and
its coefficient bits per block at every inter level are at or below ours. Our
residual spend per stream is already the LOWEST of the three (coeff+literal
43933 B vs libaom 50858 B, rav1e 50860 B) — RDOQ is not the gap; what we code
residual over is.

### 4c. mv / modes — the same motion field coded three times less precisely

| level pair | NEW% | NEAREST% | mv bits/block | mv \|max\| ≤32 | skip% |
|---|---|---|---|---|---|
| ours leaf q162 | **18.1** | 61.5 | 0.83 | 59.7% | 98.1 |
| libaom leaves q136 | **54.1** | 37.8 | **3.20** | 61.0% | 99.8 |
| ours mid ARF q142 | 33.4 | 47.6 | 2.52 | 24.7% | 89.3 |
| libaom mid q128 | 63.0 | 30.6 | **4.47** | 31.0% | 90.7 |
| ours top ARF q118 | 33.0 | 52.8 | 2.17 | 8.2% | 59.9 |
| libaom anchors q104-121 | 61-76 | 20-32 | **5.5-7.5** | — | 72.8-90 |
| rav1e L1 q104 | 26.0 | 49.5 | 2.17 | 20.9% | 80.7 |

At the leaf level the two encoders see the SAME motion field (mv magnitude
histograms agree: ≤32 is 59.7% ours, 61.0% libaom) and libaom codes a fresh
`NEWMV` on three times as many blocks, paying 3.9x our mv bits per block —
and lands at 99.8% skip for half our bytes. Across the whole stream we
underspend mv by 1545 B and overspend inter coefficients by 5633 B. Tool
fires: our `motion_mode` costs 4646 bits over 16382 symbols to win OBMC on
1212 blocks; libaom spends 7935 bits on 9200 symbols; **wedge, interintra,
intrabc, global motion and warp fire ZERO times in all three streams** on this
content (they are not the gap). rav1e codes no `motion_mode` at all and codes
`segment_id` on every frame (8149 bits, 1.4%); we code neither segmentation
nor `delta_q`.

## 5. Long-GOP, ours vs rav1e (the closer reference at 48 pictures)

Per level, at matched rate (ours 71601 B/45.444, rav1e 75452 B/45.751):

| level | ours B/frame | ours dB | rav1e B/frame | rav1e dB | verdict |
|---|---|---|---|---|---|
| key | 15827 | 47.34 | 20763 | **47.95** | ours 24% cheaper, **0.61 dB worse** — under-quality |
| top anchor | 6488 (×6, dist 8) | 45.75 | 3871 (×11, dist 4) | **45.97** | **+68% bytes for −0.22 dB — the overspent level** |
| mid anchor | 1229 (×6) | 45.63 | 537 (×12) | 45.48 | **+129% bytes for +0.15 dB — overspent** |
| leaf | 237 (×35) | 45.31 | 178 (×24) | **45.69** | +33% bytes for −0.38 dB — overspent AND under-quality |

Named: **the top-ARF level is where the long-GOP bytes are overspent** (55.3%
of our stream; rav1e reaches a better picture at 60% of the per-frame cost),
and **the key frame is where we are under-quality** (0.61 dB below rav1e's,
and every level under it inherits that). The two are one decision: rav1e puts
28% of its stream in the key and coasts; we put 22% in the key and 55% in six
anchors that are still 0.22 dB below rav1e's cheaper ones. Note this is NOT
the pyramid-shape axis that six lanes have already refuted (mini-GOP length,
level count, q offsets) — the per-frame cost of OUR anchor at a FIXED shape is
what reads 1.68x.

## 6. Lever proposals (at most 3, none built here)

### L1 — 128x128 superblock roots, after fixing the ref-slot refusal (biggest measured number)

* **Gap targeted**: 10456 B = 14.6% of the 48-frame stream ≈ **0.24 dB** of
  the 1.10 dB deficit; 38754 coded blocks against libaom's 16178 at identical
  per-block mode cost (3.26 bits both). At the leaf level 85.5% of our blocks
  are already at the 64x64 maximum, so no partition tuning can reach it.
* **Mechanism**: `use_128x128_superblock = 1` so a skipped 128x128 region
  costs one block instead of four.
* **Files**: `crates/ec-av1/src/encode.rs` — `sb128_on` (≈5705) and the leaf
  reference-slot selection the refusal names; `crates/ec-av1/src/filter_search.rs`
  for the loop-restoration unit size the flag forces to ≥128.
* **Expected BD**: the syntax saving alone is ~14% bytes on the long-GOP film
  B row (+86.7 vs libaom, +7.3 vs rav1e today) minus whatever the forced LR
  unit costs; lane-sb128 measured that cost as −0.3 dB on FILM A's 12-frame
  row, which is the same order as the gain, so the arm must be run with
  `EC_AV1_LR=0` on both sides to attribute it.
* **State found here (new)**: `EC_AV1_SB128=1` **panics deterministically on
  film B**, at 12 and at 48 frames, on the first leaf — `Leaf frame at order 3
  (last slot 0, self 0, altref 4, last2 None, shown true, golden true, altref
  pic true): a reference frame selected with no picture at this frame's own
  ref_frame_idx slot`. The memo's "intermittent" desync is not intermittent on
  this content, and lane-sb128's refutation was therefore never measured on
  the clip this gap is largest on.
* **Charter**: reproduce the refusal from `enc_probe <film B> gate 0 12 150`
  with `EC_AV1_SB128=1` (3 min, no gate needed), fix the ref-slot bookkeeping
  under a 128 root, then measure four arms at the 12-frame AND 48-frame film B
  points: sb128 off/on × `EC_AV1_LR` on/off, each judged at matched bytes
  through its own rate/quality slope, plus the census's block count and mode
  bytes as the mechanism witness (class `gate-blind-to-feature`: a 128-root
  fire counter, not a byte claim). Keep only if the film B long-GOP row
  improves against BOTH references with screen and bars flat; if the LR unit
  is what loses, the follow-on is the LR unit search under a 128 SB, not the
  128 root itself.

### L2 — the block-level NEWMV-vs-NEAREST margin (prediction precision)

* **Gap targeted**: +5633 B of inter coefficients and −1545 B of mv at matched
  rate; NEWMV share 18-33% against libaom's 54-76% at every level, on the SAME
  motion field (leaf mv magnitude histograms agree within 1.3 points); mv bits
  per block 0.83/2.17/2.52 against libaom's 3.20/4.47/5.5-7.5. Worth up to
  0.13 dB directly plus whatever the skip area recovers (our top ARF is 59.9%
  skip at distance 8 where libaom holds 72.8% at distance ~30).
* **Mechanism**: the RD margin that lets a block take `NEAREST/NEARMV` plus
  residual instead of paying for a coded mv. Not the search early-outs —
  `EXTRA_NEW_SKIP_MARGIN` and `LEAF_SECOND_NEW_MARGIN` are already swept to
  their floors — but the mv RATE term inside the block decision, the same
  shape `EC_AV1_SPLIT_LAMBDA` gave the split trial.
* **Files**: `crates/ec-av1/src/encode.rs` `search_inter_block` (9411) and the
  `symbol_bits(&cdf::NEW_MV[..])` prices at 9074/9133/9597/9681;
  `crates/ec-av1/src/motion.rs` `mv_bits` (184) for the search-side weight.
* **Expected BD**: a rate-term sweep is cheap and its ceiling here is ~0.13 dB
  (≈8% bytes) on the direct term; the class warning applies —
  `rd-rate-term-calibration` sweeps have come back single-peaked at the
  control twice (lane-txd, lane-split), so the arm that matters is the one
  that also moves the SKIP area, not just the mv share.
* **Charter**: add `EC_AV1_MV_LAMBDA` multiplying ONLY the mv-rate term of the
  block-level inter decision (search weight left alone, so the candidate set
  is unchanged), sweep 0.5/0.75/1.0/1.5/2.0 on the 12-frame film B row, and
  for each point record NEWMV share and skip area from the census beside the
  BD number — a point that moves BD without moving the NEWMV share is
  measuring something else. Take the best point to the 48-frame row and to
  screen/bars before keeping. If the sweep is single-peaked at 1.0 again, the
  finding is that our search, not our pricing, cannot find the vectors libaom
  codes, and the follow-on is subpel refinement depth at the ARF level.

### L3 — per-SB `delta_q` from the tpl map (the last unbuilt allocation mechanism)

* **Gap targeted**: not separable into bytes, but its footprint is: libaom
  codes 988 `delta_q` symbols = 2031 bits = **0.36% of its stream** in every
  inter frame and holds its whole 48-picture window inside
  [45.08, 48.56] dB with leaves 0.72 dB under its key; ours has no `delta_q`
  path at all and falls monotonically 47.34 → 45.31 (2.03 dB). Its leaves are
  +1.02 dB at half our bytes.
* **Mechanism**: move quality WITHIN a frame toward the blocks that propagate,
  using the tpl map the pyramid path already builds (lane-arfcen). Six lanes
  have refuted moving quality BETWEEN frames; this is the axis none of them
  touched.
* **Files**: `crates/ec-av1/src/encode.rs` frame-header `delta_q_present` and
  the per-superblock delta write; the tpl map from lane-arfcen.
* **Expected BD**: unpriced — the honest statement is that the syntax costs
  0.4% and the allocation it enables is the only mechanism libaom uses in
  every frame that we use in none.
* **Charter**: write `delta_q_present`/`delta_q_res` in the inter frame header
  and one `delta_q` per superblock derived from the tpl map's propagation
  weight (libaom's `deltaq-mode 1` shape: finer q where the block is
  referenced more), clamped to ±a quarter of the base qindex. Land the decode
  witness first (a stream that codes a non-zero per-SB delta and round-trips
  through ffmpeg AND `decode_stream`), then a fire counter, then the 12-frame
  film B row, then long-GOP. Do NOT re-open the pyramid offsets in the same
  lane: this must be measured at the shipped shape or it becomes the seventh
  structure lane.

## 7. Instrument added

`EC_CENSUS_TABLES=<n>` in `examples/syntax_census` — how many CDF tables the
`top tables` line names (default 8, unchanged when unset). The default hid the
mode family's own split: a level can spend 41% of its bits on `mode` with no
mode table in its top 8, which is exactly the ranking §4a needed. Zero cost
when unset.

## 8. What this lane did NOT measure

`deferred: the LR-unit attribution of the 128 root (EC_AV1_LR=0 arms) — the
knob is test-build-only (`restoration_enabled`, encode.rs:983) so it cannot be
armed through enc_probe, and EC_AV1_SB128 panics on film B before any stream
exists. Unblocked by the ref-slot fix in L1's charter plus a test-binary arm.`

`deferred: film A and the screen capture at matched rate — this lane's charter
is film B; the ranking above is a film B ranking, and libcen's film A census
(same shape, +38.9%) is the control that says the mechanism is not clip-only.`
