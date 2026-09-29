# lane-av1readcensus — the 4:4:4 coefficient-read count asymmetry, settled

**Branch:** `lane-av1readcensus`, worktree `~/.cache/wt/av1readcensus`, stacked on
`lane-av1chrtx` (`ff7cf47c`, whose `d791c464` threaded the real plane index into
`read_coeffs_rect`'s traces — the census pairs on that).

**Verdict: COUNTING SHAPE — and not even that. On every pixel-exact 4:4:4 arm
there is no asymmetry at all: both sides walk the same number of units, in the
same order, with the same plane, shape, eob and all-zero flag. The recorded
636-vs-1086 / 2052-vs-1309 signal came from an arm that DIVERGES, where the
surplus is the divergence itself, not a counting-shape difference.**

## 1. The signal, and why the census was admissible only on a pixel-exact arm

`av1444rect` round 4 recorded 636 square chroma reads against the oracle's 1086,
and 2052 luma reads against its 1309, on `444_intrabc_rect4_witness`, and filed
it as "most likely a counting-shape difference … it is NOT established either
way". Two things had to be settled before counting anything:

- the arm must be **pixel-exact**. On a diverging arm the counts are polluted by
  the divergence: a decoder reading the wrong coefficients also walks a different
  number of units, so the count difference is a *symptom*, and the count cannot
  distinguish cause from effect. Measured: `444_intrabc_rect4_witness` is wrong in
  390380 of 921600 samples, and `444_leaf8_oob` in 281306 of 737280.
- the pairing must not assume either side's own labels.

Measured exactness of every 4:4:4 fixture in the crate (ours vs `aomdec
--rawvideo`, plane-concatenated, u16→u8):

| fixture | pixels | pixel-exact? |
|---|---|---|
| `ll444_minp8_inter` | 6 frames | **yes** |
| `444_quad_leaf_tx_type` | 4 frames | **yes** |
| `444_lossy_rect4_inter_witness` | 2 frames | **yes** |
| `444_rect_strip_leaf_tx_type` | 3 frames | **yes** |
| `444_sb128rect_lr_witness` | 3 frames | **yes** |
| `444_intrabc_rect4_witness` | 1 frame | no (390380 / 921600 wrong) |
| `444_leaf8_oob` | 3 frames | no (281306 / 737280 wrong) |

## 2. Method — the census, both sides, same frame

**Ours.** A new env-gated rung `EC_CENSUS_UNIT` (`EC_CENSUS_UNIT=1`), one line
per coefficient unit **this decoder walks** — one per `read_coeffs` /
`read_coeffs_rect` call, which is exactly the object the round-4 count measured.
Fields: `n` (sequential index), `plane`, `w`, `h`, `tx`, `eob`, `all_zero`,
`entry_rng` (msac `range` before the unit's `txb_skip` read), `post_rng` (after
the unit). The plane comes from a thread-local stamped by the plane-owning
readers (`read_plane`, `read_inter_plane`, `read_inter_plane_rect`) and by
`read_coeffs_rect` from the real `plane` argument lane-av1chrtx already threads
in — a thread-local rather than a `read_coeffs` parameter because the three
luma-only call sites (`decode_rect_split`, `decode_block_rect64`,
`read_inter_plane_rect`'s corner) belong to other lanes.

**Oracle.** No rebuild needed: the instrumented aomdec already brackets every
`av1_read_coeffs_txb` call with `EC_COEFF …rng=<pre>` on entry and
`EC_COEFF_VAL …rng=<post>` on exit (`EC_TRACE_COEFF=1`), with
`EC_COEFF_STEP tag=all_zero` / `tag=eob` inside the bracket. That is the same
five fields.

**Pairing: by `post_rng`, never by plane label and never by walk position or
`n`.** `post_rng` is the msac state after the unit, so two units that consumed
the same bits end at the same state. This matters concretely here: pairing by
`n`/position invents matches the moment the two walks order a U and a V unit
differently, and pairing by plane label invents them whenever a walk's plane
attribution is what is in question. The walk is a two-pointer merge on `post_rng`;
a unit is emitted only when the two sides agree on the pairing key.

**Buckets** (each difference lands in exactly one):

- **(i) SAME UNIT** — the two sides' units pair, and plane/shape/eob/all-zero agree.
- **(ii) OUR-ONLY** — we walk a unit the oracle does not, at that bit position.
- **(iii) ORACLE-ONLY** — the oracle walks one we do not.
- **(iv) SIZE/ORDER** — the units pair but disagree on shape, plane label, eob
  or all-zero.

## 3. Per-bucket table (whole stream, paired by `post_rng`)

| arm | pixels | units ours / oracle | (i) | (ii) | (iii) | (iv) |
|---|---|---|---|---|---|---|
| `ll444_minp8_inter` (6 frames) | exact | **11292 / 11292** | **11292** | 0 | 0 | 0 |
| `444_lossy_rect4_inter_witness` (mixed shapes) | exact | **1373 / 1373** | **1373** | 0 | 0 | 0 |
| `444_quad_leaf_tx_type` | exact | **192 / 192** | **192** | 0 | 0 | 0 |
| `444_rect_strip_leaf_tx_type` | exact | 591 / 591 | 587 | 0 | 0 | 4 |
| `444_sb128rect_lr_witness` | exact | 467 / 467 | 463 | 0 | 0 | 4 |
| `444_intrabc_rect4_witness` | **diverges** | 2740 / 2395 | 1909 | 823 | 478 | 8 |
| `444_leaf8_oob` | **diverges** | 1778 / 1678 | 1027 | 751 | 651 | 0 |

Per-plane split on the primary arm, both sides identical:

| | luma | u | v |
|---|---|---|---|
| units (ours) | 3764 | 3764 | 3764 |
| units (oracle) | 3764 | 3764 | 3764 |
| of which coded (`txb_skip == 0`) | 975 | 1793 | 1859 |

So the round-4 asymmetry does not survive on an arm whose pixels are right.
Reproduced exactly on the diverging arm: `444_intrabc_rect4_witness` reads 1518
luma / 611 / 611 against the oracle's 1309 / 543 / 543 — the luma 1309 and the
per-plane 543/543 are *the round-4 oracle numbers*, so the count was never a
counting artefact. It was a real count, taken on an arm that does not decode.

## 4. The eight (iv) entries on the exact arms are a tracing artefact, not a defect

On `444_rect_strip_leaf_tx_type` and `444_sb128rect_lr_witness`, 4 units each
bucket as (iv). All four have **identical `entry_rng`, `post_rng`, `eob` and
all-zero flag on both sides** and differ only in the reported extent:

```
idx 202: ours plane=0 32x32 eob=0 az=true | oracle plane=0 txsz=TX_64X64 eob=0 az=true | post=41441
idx 498: ours plane=0 32x32 eob=0 az=true | oracle plane=0 txsz=TX_64X64 eob=0 az=true | post=33499
```

Both sides read exactly one `txb_skip` symbol and consumed the same bits; ours
reports the *corner* it decoded (the 32×32 low-frequency corner of a 64×64
luma block — `read_coeffs`' `rect_shape` path, spec 5.11.40) while libaom's
`EC_COEFF` prints the `TX_64X64` it was called with. A shape **label**
difference on an otherwise identical unit, not a walk difference. Two of the
four also carry a plane-label difference with the same entry/post rng — the same
class. Under the ticket's own rule ("a real divergence requires a unit in (i) or
(iv) whose **eob/values** differ") these are neither: no eob and no value
differs anywhere on any pixel-exact arm.

## 5. Gate

`a_pixel_exact_444_stream_walks_the_same_coefficient_units_the_oracle_does`
(`crates/ec-av1/src/stream.rs`) pins the walk's object shape on two
pixel-exact 4:4:4 arms:

- `ll444_minp8_inter` — all-TX_4X4 (min-partition-8 lossless): `[3764, 3764,
  3764]` units, `[975, 1793, 1859]` coded;
- `444_lossy_rect4_inter_witness` — **mixed shapes** (4×4, 8×8, 8×4, 4×8, 4×16,
  16×4, 8×16, 16×16): `[629, 372, 372]` units, `[130, 253, 278]` coded.

The second arm is not redundant: the first is uniformly 4×4 and would not notice
a var-tx walk that started folding 8×8 leaves. Both numbers are the oracle's own
census on the same stream.

**Mutation-proven.** Dropping 8×8 units from the walk turns it red by name:

```
the mixed-shape 4:4:4 walk changed -- [571, 313, 313] against the oracle's [629, 372, 372]
  left: [571, 313, 313]   right: [629, 372, 372]
```

and the all-4×4 arm is untouched by that mutation — which is exactly why both
arms are pinned.

**4:2:0 control.** The census is armed per frame from the sequence header's
subsampling (`set_census_444(ss_x, ss_y)` in `stream.rs`, beside
`set_mono`/`set_subsampling`), so at 4:2:0/4:2:2 every counter stays zero; the
gate asserts exactly `(0, 0, 0)` for both the unit and coded-unit counters on a
committed 4:2:0 stream that decodes. The accounting is separate from the sibling
counter pair the ticket mentions.

## 6. What this settles for the next lane

- The 636-vs-1086 / 2052-vs-1309 signal is **retracted**: it was measured on
  `444_intrabc_rect4_witness`, which is wrong in 390380 of 921600 samples. Do not
  carry it forward as a walk-shape hypothesis.
- Counting the coefficient units on a **pixel-exact 4:4:4** arm reproduces the
  oracle exactly, per plane, per unit. The census is now a one-command check:
  `EC_CENSUS_UNIT=1 cargo run -p ec-av1 --example decode_probe -- <arm>.obu`
  against `EC_TRACE_COEFF=1 aomdec`, paired by `post_rng`.
- If a future lane sees a count asymmetry, the first question is whether the arm
  decodes — not whether the walk shape differs. That check is now the gate.

## 7. Not fixed here, by design

The 4:4:4 intra-BC chroma divergence (`444_intrabc_rect4_witness`, 390380 /
921600) is untouched: this lane's deliverable is the census. Per §4 there is no
value-level divergence on any pixel-exact arm for this census to point at, so the
remaining fork is not a coefficient-read-count problem and needs a fresh charter.

## 8. Method notes for whoever repeats this

- **Pair by `post_rng`.** Position/`n` pairing invents matches when a walk
  orders U and V differently; plane-label pairing invents them when plane
  attribution is the thing in question.
- **A pixel-exact arm is mandatory.** On `444_intrabc_rect4_witness` the counts
  diverge by 823/478 units *because* the decode does; the count difference is a
  symptom, not a cause.
- **`EC_SYMR` is still the right rung for symbol-level work**; `EC_COEFF_STEP`
  is filtered (the oracle prints `tag=tx_type` for all three planes but reads it
  only for Y), so it is not a per-unit bracket. `EC_COEFF`/`EC_COEFF_VAL` are
  the per-unit bracket and were already in the oracle build — no rebuild needed.
- Our own rung must carry the real plane: `read_coeffs` had no plane parameter,
  and a stamp inherited from whichever reader ran last mis-attributes chroma as
  luma (this bit the census's first run and is the same trap `d791c464` fixed for
  the rect trace).
