# lane-last2 — a second past reference (LAST2) on the pyramid

> **HANDOVER (session closed mid-lane).** What is DONE: the DPB/slot
> bookkeeping, the search offer at 32x32, the witness, and both 12-frame
> arms on both films — the lever is REFUTED on the gate and ships OFF
> (`LAST2 = false`, `EC_AV1_LAST2=1` turns it on). What is STILL OWED:
> (a) the three suite split arms (`--skip stream::` / `stream:: --skip 10bit`
> / `10bit`) were never run on this branch; (b) `cargo check --workspace
> --all-targets` was green BEFORE the last two commits (the `EC_AV1_LAST2_NEW`
> knob and the gate's reference census) — re-run it; (c) the preset-6
> invariant arm failed ONLY on
> `the_encoders_own_streams_are_byte_identical_to_their_pins` (q=150: 9846 vs
> the 8562 pin) with `EC_AV1_LAST2=0`, i.e. with every one of this lane's
> paths switched off — the pins are DEFAULT-preset pins and `EC_AV1_SPEED=6`
> moves the stream, so this is believed pre-existing and NOT this lane's, but
> it was not confirmed against main; confirm it there first. Preset 0 with
> `EC_COMP_MISMATCH=1` passed all six invariants including the pins.
> (d) the compound `LAST_LAST2` step (charter step 4) was not built: with
> LAST2 winning only 1.3–1.7% of inter blocks as a single reference and both
> single-ref arms losing, its ceiling is too small to spend the wall on.
> No long-GOP arm was run because nothing ships (same reasoning as lane-refs).

Worktree `edith_codecs-last2`, branch `lane-last2` off main e6df5196.
Charter: `lanes/refs.report.md` §6. Every gate arm is the prebuilt release
lib-test binary running `bd_rate_screen_native` (12 pictures, `gop 12`, four
quantizers, native `gate_crop` window), one film per arm
(`EC_AV1_NATIVE_FILM` / `EC_AV1_NATIVE_FILM4K` / `EC_AV1_NATIVE_SCREEN`); every
row is read off that log's own header line.

## 0. The charter's ARF step is a stale premise (read this first)

The charter orders "ARF level first". At the ARF level there is nothing to
build: a group's top ARF ALREADY names two distinct past anchors --
`last_slot` = `ANCHOR_SLOTS[anchor]` (the previous group's top ARF, lag
`mini_gop`) and, since lane-arfcen's `arf_altref` shipped ON,
`altref_slot` = `ANCHOR_SLOTS[1 - anchor]`, which still holds the top ARF from
TWO groups back (lag 2 * `mini_gop`) when the frame is coded. Those are
exactly the census's (4, 8) ARF lags, and that reference is offered to the
32x32 search through the `extra` path WITH its own `NEWMV` search
(`EXTRA_REF_NEW_MV` = true, margin `EXTRA_NEW_SKIP_MARGIN` = 0.35). So the
census's ARF row (film A 10.31%, film B 8.38% of prediction SAD) is a lever
already banked by `arf_altref`, not an open one; naming the same slot again as
`LAST2_FRAME` would offer the same picture twice.

The open half of the census is the LEAF chain (film A 6.99%, film B 7.25%):
a leaf has exactly one past picture in the DPB (`LEAF_SLOT`, or the mid /
quarter hidden frame behind it) plus the key as `GOLDEN`. That is what this
lane builds.

## 1. The slot map

| slot | holds | written by | read as |
|---|---|---|---|
| 0, 7 (`LEAF_SLOTS`) | the last two shown leaves | every leaf, alternating | `LAST_FRAME` and `LAST2_FRAME` of the next leaf |
| 1 (`GOLDEN_SLOT`) | the key frame | the key | `GOLDEN_FRAME` everywhere |
| 2 (`MID_SLOT`) | the mid ARF | the mid ARF | `LAST`/`ALTREF` of the leaves and quarters around it |
| 3, 4 (`ANCHOR_SLOTS`) | this group's anchor and the next | the key, each top ARF | anchor = `LAST`, next = `ALTREF` |
| 5, 6 (`QUARTER_SLOTS`) | the two quarter ARFs (off by default) | the quarter ARFs | `LAST`/`ALTREF` of the leaves beside them |

Leaf `k` refreshes `LEAF_SLOTS[k % 2]`, i.e. the slot leaf `k - 2` wrote --
read before write. `Av1Encoder::leaf_hist` carries the last two leaf slots
ACROSS mini-GOP boundaries (the first leaf of a group reads the anchor as
`LAST`, so its `LAST2` is the previous group's last leaf) and is cleared by
every key frame (which writes all eight slots with one picture `GOLDEN`
already names). A leaf whose `LAST` is a hidden frame reads the previous leaf
as `LAST2`. The hidden levels pass `None`: see §0.

## 2. What changed

`crates/ec-av1/src/encoder.rs`
* `LEAF_SLOTS = [0, 7]`, `Av1Encoder::leaf_hist` (the last two leaf slots,
  cleared by every key frame), the leaf loop alternates `self_slot` and names
  the older slot; `encode_pyramid_inter` takes `last2_slot: Option<u8>`,
  drops it when it holds no picture or the same one `LAST` names, and derives
  `sign_bias[1]` / `order_hints[1]` from it through the existing `slots` array
  (so the writer and the decoder scan the same stacks).
* `last2()` / `set_last2` — `EC_AV1_LAST2`, default OFF this lane, plus the
  process-global override the witness uses (this crate's tests never call
  `set_var`).

`crates/ec-av1/src/encode.rs`
* `PyramidFrame::last2_slot`; `encode_inter_frame` takes `last2:
  Option<&Picture>` and, when the slot is named, sets
  `header.ref_frame_idx[1]`.
* The picture reaches: the 32x32 `extra` search list (`NEAREST`/`NEAR`/
  `GLOBAL` plus the existing `EXTRA_REF_NEW_MV` search under
  `EXTRA_NEW_SKIP_MARGIN` — the charter's "free arm" and its seeded arm are
  the SAME code path here, since extra references already get a NEWMV), the
  OBMC neighbour picture table, and the trial decode's `RefPix` slot 2.
* The RD pricer's `single_ref_bits` gained LAST2's `p4 = 1` leaf.
* The native gate prints a per-clip reference census (`LAST / LAST2 / GOLDEN /
  ALTREF`), so the keep table can see whether the tool fires at all.

The 16x16/8x8 leaves are NOT offered LAST2 as a single reference: that path
(`code_square_inter`) takes no `extra` list at all, only compound pairs. The
64x64 root goes through `search_skip_64`, likewise no `extra`. So this arm is
the 32x32 level, which is where the existing extra references live too.

## 3. The witness

`encoder::tests::a_leaf_predicts_off_last2_when_the_picture_two_back_matches`
(not ignored, 3.6 s): nine 128x128 pictures ALTERNATING between two textures,
`mini_gop` 4, so every leaf's picture two back is an exact match.

| lever | LAST | LAST2 | GOLDEN | ALTREF | stream |
|---|---|---|---|---|---|
| off | 67 | **0** | 18 | 8 | 2969 B |
| on | 25 | **36** | 15 | 8 | 2300 B |

36 blocks coded off LAST2 (0 with the lever off — the counter measures this
lane and not the reference set we already had), −22.5% bytes on the clip the
lever is built for, `decode_stream` and ffmpeg both reconstruct all nine
display positions sample-exact, and one `show_existing_frame` per hidden frame
survives.

## 4. The 12-frame native gate

Controls reproduce to the digit: film A +21.7 / −4.4, film B +26.9 / −0.6.

| arm | film A (vs libaom / rav1e) | film B | wall ours, film A / film B |
|---|---|---|---|
| control (`EC_AV1_LAST2=0`) | +21.7 / −4.4 | +26.9 / −0.6 | 208.2 s / 165.0 s |
| LAST2 at the 32x32 search, with its own NEWMV | +21.4 / −4.5 | **+27.3 / −0.1** | 242.0 s / 202.5 s |

Film A moves 0.3 down on the libaom column and 0.1 UP on rav1e's; film B moves
0.4 and 0.5 the WRONG way on both. The keep rule ("both film rows improve on
both columns, or one ≥0.5 down and the other flat ±0.3") is not met — and the
arm costs +16% / +23% encoder wall, on the stage the user's speed order is
about. So the searched arm is refuted.

| LAST2 search-FREE (`EC_AV1_LAST2_NEW=0`) | +21.5 / −4.5 | **+27.3 / −0.0** | 194.3 s / 171.3 s |

The charter's "free arm" does not rescue it: film A is 0.2 down on libaom and
0.1 up on rav1e, film B is 0.4 / 0.6 the wrong way on both columns. The wall
comes back (194 s / 171 s against the control's 208 s / 165 s, i.e. inside the
gate's own wall noise), so the cost of the searched arm was indeed the second
motion search — but the reference itself does not pay for its own syntax.

**Fire share, on the real film rows** (the census the gate now prints):

| clip | LAST | LAST2 | GOLDEN | ALTREF | LAST2 share |
|---|---|---|---|---|---|
| film A | 41871 | 780 | 1349 | 916 | 1.7% |
| film B | 39412 | 566 | 1894 | 1807 | 1.3% |
| bars 1080p | 22626 | 58 | 4067 | 206 | 0.2% |

So the tool DOES fire on real film (it is not a dead knob — the witness and
these counters are the [[gate-blind-to-feature]] proof), it is simply chosen
by 1 block in 60 and those blocks do not pay back the `single_ref` symbol they
spend. Read against `refs.report.md` §1's census (38% of blocks preferring the
older picture by SAD): a pure-SAD best-of-two over-counts the lever by more
than an order of magnitude once the block also has NEAR/NEAREST/DRL off LAST,
a backward `ALTREF` at every leaf, and a rate term.

## 5. Decision

`LAST2` ships **OFF** (`const LAST2: bool = false`). The machinery stays: it
is the only way the census's remaining claim can ever be re-measured, it is
witnessed end to end, and it costs the default path nothing (a `None` picture
is filtered out of the `extra` list and the header is untouched — the pins are
byte-identical with the lever off).

## 6. Deferred / dropped

* **The compound `LAST_LAST2` pair (charter step 4) — dropped, not deferred.**
  Its input is the single-reference arm, which loses on both films with a
  1.5% fire share.
* **The ARF-level LAST2 (charter step 1) — dropped as a stale premise**, §0.
* **The RD pricer charges `ALTREF` as if it were `LAST`** (`single_ref_bits`,
  `encode.rs`): a backward reference codes `p1=1,p2=1`, two different symbols
  from LAST's `p1=0,p3=0,p4=0`. Left alone here because fixing it moves every
  shipped stream, so it needs its own gate arm — deferred, unblocked by a lane
  that can re-pin. Found while adding LAST2's own leaf to that same tree.
