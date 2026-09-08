# lane-txi -- the intra type search's film loss, and `reduced_tx_set = 0`

Branch `lane-txi` off main `f6c67770`. Predecessors: `lanes/txset.report.md`
(intra five-type set, screen-gated), `lanes/txset2.report.md` (inter
`DCT_IDTX`, screen-gated), `lanes/txrd.report.md` (the inter chroma-inherit
fix).

## 1. The instrument: WHERE the intra unconditional arm loses (12-frame gate)

`EC_AV1_TXSET=all` is new here -- it lifts the intra search's screen gate
without touching the preset lever -- and the gate already prints per-frame AND
per-plane PSNR under `EC_AV1_FRAME_PSNR=1` (lane-txrd's instrument).

Control and arm both REPRODUCE the charter's numbers to the digit: bars 1080p
-1.0/-16.8 -> -2.3/-17.8, film A **+21.7/-4.4 -> +22.1/-4.0**.

film A, per plane, per frame (frame 0 is the key frame; q=60 shown, q=150
identical in shape):

| plane | control | arm (`EC_AV1_TXSET=all`) |
|---|---|---|
| Y | 51.72 46.53 47.40 46.51 47.10 47.09 46.50 ... | **51.73 46.55 47.42 46.52 47.13 47.12 46.52 ...** |
| U | 52.52 49.39 49.75 49.44 49.65 49.87 49.60 ... | **52.52 49.41 49.77 49.49 49.68 49.90 49.62 ...** |
| V | 52.39 49.26 49.79 49.25 49.65 49.45 49.08 ... | **52.39 49.27 49.81 49.28 49.67 49.44 49.10 ...** |

READING, and it refutes the charter's premise: the arm is EQUAL OR BETTER on
every frame, every plane and both quantizers shown -- the key frame included,
the leaves included. There is no plane collapse (lane-txrd's chroma class) and
no propagation shape (the leaves do not decay relative to the key frame), so
**a reference-aware `lambda` term has no signal to act on and was not built**.

What the arm actually spends is BYTES: 64392 -> 65201, 102508 -> 103781,
170169 -> 172167, 413430 -> 419156, i.e. +1.2..1.4% of rate for +0.02..0.06 dB.
That is a rate term the type decision under-charges (class
`rd-rate-term-calibration`), not a distortion it mis-measures.

DEVIATION FROM THE CHARTER, stated: instead of the reference-aware term, this
lane builds the term the instrument points at -- `encode::non_dct_surcharge`,
`(EC_AV1_TXRD_LAMBDA - 1) * lambda * bits` charged to a non-`DCT_DCT`
candidate only (the incumbent is always `DCT_DCT`, so the block's returned
cost stays its true one). The intra decision now reads the same
`EC_AV1_TXRD_LAMBDA` the inter one already did. Sweep below.

## 2. `reduced_tx_set = 0`: the writer half, and the defect that blocks it

Commit `95b4bee9`. The bit is read in ONE place on the writer side:

* `Cdfs::reduced_tx_set` (default `true`) + `TxbSet::wide()` -- every luma
  set's `Set1` counterpart, transcribed from `decode::txbset_for` /
  `txbset_for_inter` / `inter_txbset_for`, applied at the top of `Cdfs::txb`.
  The `Set1` tables differ from their narrow twins in the `tx_type` field
  ONLY, so this is the whole writer-side map; the writer's symbol already
  comes from `decode::tx_type_symbol(cdf.len(), ..)`, so the wider alphabets
  need no writer change at all. The decoder never sets the field (it names the
  wide set itself; widening a wide set is the identity).
* `encode::wide_tx_set(screen)` (`EC_AV1_TXSET_WIDE` = `0`/`screen`/`1`,
  `speed::WIDE_TX_SET` false at every preset) drives `header.reduced_tx_set`
  on both header sites, and the frame's `Cdfs` reads the header bit.
* `encode::luma_set_for` gives the SEARCH and its pricer the same widened set,
  so the two agree on what a `tx_type` symbol costs.
* Candidates: intra 8x8/4x4 get the seven-type `TX_SET_INTRA_1`; the twelve-
  and sixteen-type inter lists are written (the seven types both alphabets
  share) but NOT offered -- see below.

BLOCKER, bisected: with the bit off, the encoder's own trial decode of the
tile it just wrote (`filter_search::pick_filters` -> `decode_inter_frame_
tiles_lr`) desyncs and reads `BWDREF` on a frame that holds no such picture --
"a reference frame selected with no picture at this frame's own ref_frame_idx
slot for it", refused out of `decode::ref_dims` (new probe:
`EC_AV1_REFPROBE=1` prints the missing reference and the backtrace). Bisection:

| arm | result |
|---|---|
| wide bit + both type searches OFF | PASS (witness + ffmpeg exact) |
| wide bit + INTER search on, intra off | PASS |
| wide bit + INTRA search on (7-type set) | **FAIL, the refusal above** |
| wide bit off, `EC_AV1_TXRD_LAMBDA` 0/0.5/4 (cost perturbation alone) | PASS |

So it is not RD noise and not the inter half: something in the SEVEN-type
intra path writes a symbol the reader resolves differently. One real defect was
found and fixed on the way (`decode_inter_frame_tiles_lr` hardcoded
`reduced_tx_set = true` for the encoder's own trial decode -- the class its
neighbours' doc comments name, a header bit a raw tile decode cannot guess);
it is not the whole story, the desync survives it.

`speed::WIDE_TX_SET` is therefore false at every preset and
`EC_AV1_TXSET_WIDE` unset is byte-identical to main.

## 3. The surcharge sweep (12-frame gate, `EC_AV1_TXSET=all`, film rows)

BD-rate vs libaom `cpu-used 6` / rav1e `speed 6`, lower is better. Every arm is
the UNCONDITIONAL intra search (the shipped default is screen-gated and
byte-identical to the control on these rows).

| clip | control | 1x (the plain arm) | 2x (`EC_AV1_TXRD_LAMBDA=2`) | 0.5x |
|---|---|---|---|---|
| bars 1080p | -1.0% / -16.8% | -2.3% / -17.8% | -1.2% / -16.9% | (see below) |
| film A | +21.7% / -4.4% | +22.1% / -4.0% | **+21.7% / -4.3%** | (see below) |

film A bytes: control 64392/102508/170169/413430, 1x 65201/103781/172167/419156
(+1.2..1.4%), 2x 64518/102989/170564/414763 (+0.2..0.3%).

READING: the surcharge does exactly what the instrument predicted -- it buys
the rate back (the arm's +1.3% of bytes collapses to +0.2%) and the film A row
walks from +22.1/-4.0 to +21.7/-4.3, i.e. onto the control against libaom and
0.1 SHORT of it against rav1e. It neutralises the loss; it does not turn it
into a win, because what the search wins on film (0.02-0.06 dB) is smaller
than what its symbols cost however they are priced. The same surcharge costs
the bars row a point of its own gain (-2.3 -> -1.2), which is the sign that a
single flat weight cannot serve both content classes.

KEEP RULE: FAIL for the unconditional arm at every weight measured -- the rule
wants both film rows DOWN, and the best weight lands film A ON the control.
DECISION: the intra search stays SCREEN-GATED, the surcharge ships at its
default weight of 1.0 (`non_dct_surcharge` returns 0, so every default stream
is byte-identical to main), and `EC_AV1_TXRD_LAMBDA` stays the swept lever.

## 4. Invariants, pins, suite

Default state = every lever of this lane OFF (`WIDE_TX_SET` false at all
presets, surcharge weight 1.0, the intra/inter searches on their own screen
gates), so the shipped encoder is byte-identical to main -- which the pins
make below.

| invariant | preset 0 | preset 6 |
|---|---|---|
| `an_edge_clip_codes_every_reduced_set_tx_type_both_decoders_read_exactly` | PASS | PASS |
| `an_inter_clip_codes_both_inter_set_tx_types_both_decoders_read_exactly` | PASS | PASS |
| `the_facade_codes_the_same_bytes_as_encode_sequence` | PASS | PASS |
| `predicted_coeff_bits_track_the_tile_the_writer_wrote` | PASS | PASS |
| `tile_bytes_do_not_depend_on_the_thread_count --include-ignored` | PASS | PASS |
| the three above under `EC_COMP_MISMATCH=1` (preset 0) | 0 mismatches | -- |

`the_encoders_own_streams_are_byte_identical_to_their_pins`: PASS at the
UNCHANGED 8562 / 33357, and again under `EC_AV1_TXSET=0 EC_AV1_TXSET_INTER=0`.
`timeout 890 cargo check --workspace --all-targets -j4`: 0 errors, 0 `ec-av1`
warnings (the 22 the workspace prints are `ec-opus`/`ec-vorbis`, untouched).

`a_wide_tx_set_clip_codes_the_new_alphabets_both_decoders_read_exactly` is
`#[ignore]`d with the blocker in its reason string -- it is the witness that
closes section 2 the moment the desync is found.

LONG-GOP: not re-run, and it does not need to be -- what ships changes no
byte (pins unchanged, every lever off by default), so
`bd_rate_film_long_gop` stands at the head's own +26.4/-6.6 (film A) and
+89.8/+9.1 (film B).

## 5. Deferred

* `deferred: reduced_tx_set = 0 -- the writer half is BUILT and off by default;
  what blocks it is a desync between the seven-type intra path's writer and the
  encoder's own trial decode (section 2's bisection table), which shows up as a
  refused reference slot. What unblocks it: the first symbol where the trial
  decode's range diverges from the writer's (class equal-range-means-unread /
  compare-range-not-tell), on the two-frame witness that already reproduces it
  in 2 seconds -- one lane slot, no gate wall.`
* `deferred: the twelve/sixteen-type INTER candidate lists -- written
  (encode::inter_tx_type_candidates' INTER_WIDE) but never offered, since the
  header bit they need is the blocked one above. Unblocked by the same fix.`
* `deferred: the reference-aware type cost the charter asked for -- NOT BUILT,
  and deliberately: the per-plane per-frame instrument (section 1) shows the
  arm equal-or-better on the key frame AND every leaf, so there is no
  propagation signal for such a term to price. What would revive it: a content
  class whose leaves DO decay under the arm.`
* `deferred: the surcharge as a per-content lever (screen 1.0, film 2.0) --
  measured on film A only; film B and the capture were not swept. Unblocked by
  two more gate slots. As a flat default it is a wash, which is why it ships
  at 1.0.`
* `deferred: EC_AV1_REFPROBE=1 is a new decoder-side probe (the missing
  reference slot plus a backtrace); it is env-gated and costs a branch on an
  error path only.`
