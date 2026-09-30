# lane-av1mixllconj — the per-segment `lossless` answer ALREADY agrees with libaom; the var-tx **conjunct** lands, and the 640x480 "regression" is a SECOND, independent defect on a cell that was never correct

> **CORRECTIONS FROM THE REFUTATION PASS (added by Main 2026-09-30; VERDICT: PARTIAL — every DECODER claim
> reproduced exactly, three REPORT/doc figures did not).** The pass reproduced the pin (6/6 decode-order dumps
> byte-exact, shown rawvideo 245760 B with 0 differing bytes), the whole-stream ladder (96997 oracle lines /
> 96997 ours / 0 unparsed / 96997 paired bit-identical / first divergence NONE), the conjunct-out red
> (f1..f5 = 33724/34251/44500/36688/37462, gate FAILED at decode-order frame 1 byte 33, ours 82 vs 81, ladder
> first divergence at read 30889), the per-segment derivation, the premise reversal on base (m5_640x480 f1..f5
> = 352066/414286/313795/399471/422078 of 460800 -- the report's exact numbers, 76/70/68/87/92 % wrong), and the
> off-tile arithmetic re-derived independently. Corrections:
>
> 1. **`SUB8_LOSSLESS_NO_VARTX` was NOT the decision counter it claimed to be** (doc at
>    `decode.rs:34766-34772`, and my merge message repeated the claim): the pass measured **33 hits on a build
>    with the conjunct REMOVED**, because the `else if lossless(fctx) && !skip` arm also serves leaves whose
>    `tx_select_inter` is OFF -- leaves the conjunct does not touch. The hit is now guarded on `tx_select_inter`
>    (the same term the conjunct uses) and the pin's delta reads **10, not 40**; the gate still asserts `> 0`,
>    remains green, and its doc names both numbers. Same class as the palette counter correction: a hit that
>    fires for a population the fix does not touch is not a non-vacuity arm.
> 2. **The corpus buckets do not reproduce, and the CAUSE IS NOW DIAGNOSED rather than left as two
>    incompatible sweeps**: the lane compared OUR `EC_AV1_DECODE_ORDER_DUMP` against the oracle's
>    `EC_AV1_FINAL_DUMP`, but our dump NARROWS every plane to u8 (`stream.rs:2189`,
>    `let narrow = |v: &[u16]| v.iter().map(|&s| s as u8)` -- the code even says in a comment that it
>    "predates 10-bit support and is an 8-bit-oracle comparison only"), while the oracle's dump is
>    bit-depth correct. On an 8-bit stream that is a fair compare; on 10/12-bit it mismatches on EVERY frame
>    BY CONSTRUCTION. Nearly all 34 of the lane's RED rows were HBD fixtures
>    (`444_lossy_superres_*_10bit/12bit`, `hg_*`, `intra14_256x192_10bit`, `palette_screen_*_10bit`, ...) that
>    were "red" because u16 samples were halved, not because a pixel differed. With those out, the RED count
>    collapses to the pass's own 0 -- which is why the correct buckets are **base 106 = 96 EXACT / 0 RED / 9
>    FRAMECOUNT / 1 ORACLE_FAIL** and **tip 110 = 100 / 0 / 9 / 1**. The SUBSTANCE of the lane's sweep is
>    untouched and its zero-EXACT->RED result stands. Class recorded in the skill
>    `oracle-raw-packing-depth-audit`: an all-HBD red set is the signature of a narrowed-dump compare, and a
>    documented trap is not a neutralised trap -- the compare path has to encode the depth, not a comment.
> 3. **The cited oracle rung `EC_AV1_DECODE_ORDER_DUMP` does not exist** in the shared oracle build (only
>    `EC_AV1_FINAL_DUMP`, `decodeframe.c:5699`) -- it is OUR rung, 8-bit-only, which is the same fact as
>    correction 2 wearing a different coat. The measurement it supports was made with the decode-order dump
>    path the crate's own helpers use; the rung NAME in the text is wrong and no test depends on it.


Base `main` = `6bb66a4a`. Worktree `/home/tahinli/.cache/wt/av1mixllconj`, target dir
`$HOME/.cache/cargo-target-av1mixllconj`, `EC_NOMEMGUARD=1`. Oracle
`~/.cache/aom-oracle/build/{aomdec,aomenc}` (shared, untouched; not rebuilt, not
re-instrumented). ffmpeg 8.1.3.

**This lane lands the two-line conjunct** (cherry-picked candidate `658d37fa`,
re-derived and re-proved here) **plus its fixture, route counter and gate.** One
regression control from the charter is NOT met as literally written — the
640x480 cell refuses instead of decoding — and §4 shows, with measurements, that
the control's premise is false (the cell is 70–92 % wrong at base) and that the
refusal is caused by a **different defect class**, localised to one read and
reproduced to a one-line override.

---

## 0. Verdict

| | |
|---|---|
| The charter's live question — "make our per-segment `lossless` answer agree with libaom's `xd->lossless[segment_id]`" | **the answer is: it already agrees.** The derivation is line-for-line libaom's, and 179 378 bit-identical entropy reads on the 640x480 cell prove no var-tx gate ever consulted a wrong flag. There is nothing to fix in the derivation. |
| 256x128 pin (`--limit=5`) | **6/6 decode-order byte-exact**; paired `EC_SYMR` **96 997 / 96 997 bit-identical over the WHOLE stream** (§3.1) |
| 4:4:4 arm (`--profile=1`, 8 frames) | **9/9 exact**, red on f1..f8 before |
| 176x144 / 352x288 (refuse at base) | 352x288 → **7/7 exact**; 176x144 → decodes 7/7 (still red from the already-handed-off `mi=(32,32)` defect, which `lane-av1offtile`'s clip then closes — §7b) |
| 640x480 — **CONTROL NOT MET** | refuses at frame 3 instead of decoding. Base decodes it **70–92 % wrong on every frame** (§4.1). Refusal cause = a second defect, named and one-line-reproduced in §4.3. |
| Corpus sweep, 107 committed fixtures | **exactly one status change: `420_mixll_altref_256x128_5f` RED → EXACT.** Zero EXACT→RED, zero new REFUSED, zero FRAMECOUNT change (§5) |
| Composition with `714fb55b` (`lane-av1offtile`) | cherry-picked for measurement and reverted; the two clips close **every** cell of this recipe at 256x128 / 176x144 / 352x288 / 4:4:4 and leave the 640x480 residual byte-for-byte unchanged (§7b) |
| Class sweep | all 10 `else if lossless(fctx)` sites and all 9 `read_var_tx_size` gates re-derived against libaom's own `if`: **exactly the 2 fixed sites were in the class, no third site** (§6) |
| Fixture / gate / counter | committed; gate mutation-proven red-before (§3.2) |

## 1. Reproduction

Every cell below is `testsrc2` at 25 fps, `ffmpeg -f lavfi -i
testsrc2=size=<S>:rate=25 -frames:v <N> -pix_fmt yuv420p`, then

```text
aomenc --codec=av1 --obu -o <cell>.obu --passes=1 --cpu-used=4 --limit=<N> \
       --end-usage=q --cq-level=0 --aq-mode=1 src<S>.y4m          # 4:2:0
       ... --profile=1 ... src444.y4m                             # 4:4:4
```

| cell | bytes | sha256 (prefix) |
|---|---:|---|
| `m5_256x128.obu` (the pin) | 18 525 | `3e06b5641e6a0f5a` |
| `m_256x128.obu` | 22 336 | — |
| `m_176x144.obu` | 20 069 | `02b15d0d5b08aa5a` |
| `m_320x240.obu` / `m5_320x240.obu` | 38 686 / 28 716 | `37fbe7cb…` / `40d613fd…` |
| `m_352x288.obu` | 46 490 | `e43c74b8d847e701` |
| `m5_640x480.obu` | 87 537 | `3d505fd2e0bbb7ca` |
| `m_640x480.obu` | 115 615 | `c82ba4d793d378cc` |
| `m444_8.obu` (4:4:4, 8 frames) | 79 051 | — |

The three byte counts and shas the inherited reports name all reproduce on the
first try.

## 2. The charter's premise, tested and answered

The charter says: *"our per-segment lossless answer disagrees with libaom's
`xd->lossless[segment_id]` on that stream … the table is built from a
qindex/delta predicate that does not account for what segmentation ACTUALLY
resolves to."*

**It does not disagree. Two independent measurements:**

**(a) The derivation is libaom's, term for term.** libaom
`decodeframe.c:5210-5217`:

```c
for (int i = 0; i < MAX_SEGMENTS; ++i) {
  const int qindex = av1_get_qindex(&cm->seg, i, quant_params->base_qindex);
  xd->lossless[i] = qindex == 0 && quant_params->y_dc_delta_q == 0 &&
      quant_params->u_dc_delta_q == 0 && quant_params->u_ac_delta_q == 0 &&
      quant_params->v_dc_delta_q == 0 && quant_params->v_ac_delta_q == 0;
  xd->qindex[i] = qindex;
}
```

`av1_get_qindex` (`quant_common.c:222`) is
`segfeature_active(SEG_LVL_ALT_Q) ? clamp(base_qindex + data, 0, MAXQ) :
base_qindex`. Ours (`ec-av1-syntax/src/frame.rs:658-666` and `1022-1033`) is the
same predicate, the same clamp, the same five deltas — and it is evaluated
**after** `read_quantization_params` / `read_segmentation_params` /
`read_delta_q_params` / `read_delta_lf_params`, i.e. after segmentation has
resolved, which is the one thing the charter suspected. The suspicion is
**REFUTED**.

The table this lane prints per frame (env-gated, reverted; the 640x480 cell, all
five frames identical) shows the mixed structure the class needs:

```text
EC_MIXLL_FRAME base_q=0 deltas=(0,0,0,0,0) qindex=[0,0,0,0,1,1,1,1]
                lossless=[true,true,true,true,false,false,false,false]   # frames 0-1
EC_MIXLL_FRAME base_q=0 deltas=(0,0,0,0,0) qindex=[0,0,0,0,0,0,0,0]
                lossless=[true,true,true,true,true,true,true,true]      # frames 2-4
```

**(b) A wrong flag could not have stayed hidden.** On `m5_640x480` the parse is
**bit-identical to the oracle for 179 378 entropy reads** with the fix in. At
every var-tx gate a wrong `lossless` answer changes the NUMBER of reads (the tree
walk appears or vanishes), so a wrong flag at any gate inside that range would
have forked the ladder. It did not. **No tree-gate on this stream ever
consulted a wrong flag.**

So the charter's "fix the DERIVATION" lever is empty, and the remaining question
is the one §4 answers.

## 3. The landed change, and its proof

```diff
@@ decode.rs:27213 (intrabc 4x8 / 8x4 rect leaf, read_var_tx_size)
-            if tx_select && !skip {
+            if tx_select && !skip && !lossless(fctx) {
@@ decode.rs:48332 (sub-8 4x8/8x4 inter leaf, read_var_tx_size)
-        if fctx.tx_select_inter.with(std::cell::Cell::get) && !skip {
+        if fctx.tx_select_inter.with(std::cell::Cell::get) && !skip && !lossless(fctx) {
```

libaom's own condition (`decodeframe.c:1237`) carries
`!xd->lossless[mbmi->segment_id]` **inside** the tree condition and
`read_tx_size` (`:1203`) answers `TX_4X4` on the same flag before reading
anything. `lossless(fctx)` is already per-segment
(`lossless_per_seg[cur_segment_id]`, `decode.rs:453-456`), so the flag was right
and one branch too late.

### 3.1 Whole-stream ladder, not just "up to the fork"

`EC_SYMR=1` on both sides of the pin, comparing the convention-free fields
(`pre[0]`, `pre[1]`, `n`, `s`, `post_rng`; `pre[2]` carries a per-site constant
offset and `cdf0` the ICDF mirror, so neither is the signal):

```text
ours=96997 (unparsed 0)  oracle=96997 (unparsed 0)
bit-identical over all 96997 paired reads
```

### 3.2 Gate, counter, mutation proof

* fixture `crates/ec-av1/fixtures/420_mixll_altref_256x128_5f.obu`, 18 525 B,
  pinned by **fnv1a64** `16348502764482216576` (not a `const SHA256` compared
  against itself);
* route counter `SUB8_LOSSLESS_NO_VARTX`, incremented on the **decision** — the
  `else if lossless(fctx) && !skip` arm, i.e. it can only be non-zero when the
  conjunct actually suppressed a tree that the pre-fix tree would have walked
  (`decode.rs:48349`). A stream whose sub-8 leaves all sit in lossy segments
  leaves it at zero, so the gate cannot look armed by merely decoding;
* gate `a_420_mixed_lossless_alt_ref_sub8_vartx_witness_is_byte_exact_in_decode_order`
  asserts 6 decode-order dumps with **1 hidden**, the counter delta `> 0`, and the
  shown-frame per-plane diff against `aomdec --rawvideo`.

Mutation proof — the fix surgically reverted from the working tree, the gate
re-run — is in §8.

## 4. The 640x480 control: what actually breaks, and why it is not this class

### 4.1 The control's premise is false — the cell is wrong at base

Decode-order dumps vs the oracle's `EC_AV1_FINAL_DUMP`, 460 800 B per frame:

| cell | at base `6bb66a4a` | with the fix |
|---|---|---|
| `m5_640x480` (87 537 B) | **6/6 dumps, f1..f5 red by 352 066 / 414 286 / 313 795 / 399 471 / 422 078** | 4/6 dumps, then `REFUSED: unsupported: AV1 tile (a Golomb tail longer than this decoder reads)` |
| `m_640x480` (115 615 B) | 7/7 dumps, f1..f6 red by 162 045 … 439 643 | 5/7 dumps, then the same refusal |

So the base tree does not "decode 640x480" — it emits between 8 % and 30 % of
each frame's bytes correctly and says nothing. The prior lane recorded the same
gap (`lanes/av1mixllfork.report.md` §7: *"Whether the 640x480 base decode is red
or exact — I measured that it decodes, not that it is correct"*). It is red.
The fix converts a **silent** near-garbage decode into a **loud** refusal, which
is the direction the project's honesty invariant asks for
(`skill://edith-refusal-inventory-gates`).

### 4.2 The fix moves the fork 40 892 reads forward — it is not the blocker

Paired `EC_SYMR`, first divergence over the whole stream:

| tree | first divergence | our read count | oracle |
|---|---:|---:|---:|
| base `6bb66a4a` | **138 586** | 860 431 | 457 696 |
| with the fix | **179 378** | 521 856 | 457 696 |

The base fork at 138 586 is the *same* signature as the pin's read 30 889 — the
oracle goes `decodemv.c:191` (`ref_mv`) → `decodetxb.c:158` (`txb_skip`) with no
`tx_size` symbol in between, and we read one — i.e. **the 640x480 cell has the
same lossless defect, and the fix fixes 40 892 reads' worth of it.** The
remaining fork is elsewhere.

### 4.3 The remaining fork is a `tx_size_context` read — a different class

At read 179 378 both sides are at the **same block, `mi=(80,110)`** (the oracle's
own `EC_TXCTXB` rung names it; its `EC_SYMR` `mi=` label is stale there, and
`site=decodeframe.c:1186` is **not** the var-tx split read — the built binary's
line numbering is 17 lines behind the current `decodeframe.c`, and 1186 there is
`read_selected_tx_size`'s `tx_size_cdf` read, which a paired
`EC_SYMR=1 EC_VARTX=1` run proves: the var-tx rung fires only 15 times in the
whole stream and at none of these bit positions).

| side | above band | left band | above | left | **ctx** |
|---|---|---|---|---|---:|
| libaom (`EC_TXCTXB mi=80,110 bsize=2 maxw=8 maxh=4 hasup=1 hasleft=1`) | 4 | 8 | 0 | 1 | **1** |
| ours (`EC_TXCTX mi=80,110 own=8x4`) | **16** | 8 | 1 | 1 | **2** |

The read is `tx_size_cat0[ctx]` at `decode.rs:47264`, a 4:2:0 **8x4 intra leaf
with `use_filter_intra` inside an inter frame** — a *sub-8 intra-rect leaf*, not
an inter leaf, so neither fixed site is involved.

**One-line reproduction.** Forcing `ctx = 1` at that single site (an env-gated
override, reverted) makes the whole 640x480 cell **bit-identical to the oracle**:

```text
natural ctx=2 : ours=521856 reads, first divergence 179378
forced  ctx=1 : ours=457907 reads, oracle=457907, bit-identical over all 457907
forced  ctx=0 : 286327 reads   forced ctx=2 : 521856
forced  ctx=3 : 179378 reads   forced ctx=4 : 179378
```

**Joint fact, reproduced on two trees.** `lane-av1offtile` re-measured this on
`9abe724a` + their chroma clip (`git apply --3way`, clean) with the same
per-site override and got the same lock: `ctx 1 -> ours=457907` against
`oracle 457907`, bit-identical over all of them, while the natural value gives
521 856 with the first divergence at 179 378. Their operand reads and their
`EC_TXUPD` replay reproduce mine exactly, including the four writes to mi column
110 in that frame and the last one being the skipped 16-px inter block at
`mi=(36,108)`. Their record is `lanes/av1offtile.report.md` §6b, tip `3a9676ed`.
Three corrections their run established and I fold in here:

* the site is the `tx_size_cat0` arm of **`decode_leaf_rect8`**, and the
  override only bites **with this lane's conjunct landed** — on base + their
  clip alone the cell reads 860 431 and no `ctx` value changes anything, because
  the block is never reached in a state where it matters. Dependency order:
  off-tile chroma walk -> var-tx conjunct -> band gap.
* resolving my line number against MY tree is what caught their first attempt
  (they keyed the `tx_size_cat1` site and got five identical runs, which reads
  as "the claim does not reproduce").
* the `EC_SYMR` comparator's third field must accept a **negative**
  `pre[2]`: 211 of this stream's 457 907 oracle lines carry one, and a `\d+`
  third field drops them, which is the same silent under-count that made my own
  first pass read "96945 paired" instead of 96997 on the pin.

No forced-`ctx` production code is in either branch.

**Re-measure protocol — the operand pair, not the pixel run, is the
discriminator.** When the band write lands, a green 640x480 pixel compare
establishes only that the cell is exact; it cannot say WHICH cause closed,
because the ctx=1 lock would produce the same green. Assert on the two rungs
instead, on the same tree, after the fix:

```text
oracle  EC_TXCTXB mi=80,110 abv=4 lft=8 above=0 left=1 ctx=1
ours    EC_TXCTX  mi=80,110 above_txfm=? left_txfm=8 above=? left=true
```

Three outcomes, and they are genuinely distinct:

| `above_txfm` after the fix | `ctx` after | what it means |
|---|---|---|
| 4 | 1 on its own | the band write at `mi=(36,108)` was publishing 16 where libaom has 4; the fix closed the band and the ctx=1 lock is **retired** as a symptom mask. This section's chain is confirmed end to end. |
| **16** | 1 anyway | the fix was in the ctx formula or the arm, **the band is still wrong**, and the cell going exact is the mask doing the work rather than a repair. This is the case worth catching: the pixel run alone reports it as success. |
| 4 | not 1 | something else is still there and the cell should NOT go exact. If it does, a further defect is being masked too. |

This is an ASSERTION, not a log line, precisely because the second outcome is the
one a green pixel compare hides. The cheaper first discriminator is the diff
itself: a band-write site touched with `decode_leaf_rect8` untouched means the
band was the cause; the reverse means the band was a symptom and §4.3 was
directionally right but causally wrong. `git diff --stat` answers it without a
decode run at all.

**The off-tile link is evidence for the ORDER, not just a step in it.**
`lane-av1offtile`'s chroma-plane walk moved the 176x144 first divergence from
25 792 to 29 411 and changed **nothing at all** on 640x480; the var-tx conjunct
is what made `mi=(80,110)` reachable in a state where its context matters at all
(with base + their clip alone the cell reads 860 431 and no `ctx` value moves it).
Those two facts are what make the chain *ordered* rather than merely
*sequential*, and they are the reason the operand pair is readable now.

**Standing obligation, recorded so the next reader knows this number is expected
to move:** when Kerem-8 lands the band write, this lane re-measures `m5_640x480`
on the combined tip and updates the §0 verdict row and this section's cell status
from `REFUSED` to byte-exact. `lane-av1offtile` has taken the same obligation for
their §6. The cell status is the ONE number in this report that a later commit
can legitimately change; nothing else here depends on the band write.

**Reconciling the two override sweeps.** My first sweep forced the context
**globally** (every `tx_size_cat0` read in the stream), which is why its
non-answer rows differ from the per-site sweep's: a global override perturbs
every site of the category and forks earlier, while a per-site override touches
only the fork's block. The **answer row is the same in both** — `ctx=1` makes the
stream bit-identical — so the two tables agree where it matters and differ only
where they were asked different questions. A ladder comparison should always say
which sweep it ran.

**The band itself is wrong, not the context formula.** Replaying every
`EC_TXUPD` band write up to read 179 378, the last write to mi column 110 is a
**skipped** 16-px-wide inter block at `mi=(36,108)` (`skip_inter=true`,
`w_mi=4 → bw=16`) at read 157 577; nothing writes column 110 between then and
the read. libaom reads 4 there, so libaom has a band write in rows 37..79 that
this tree does not make — class `context-band-not-published`, the same class
`decode.rs:27595`'s comment already names. **Not this lane's hunks, and not
fixable from a one-line context change: a forced `ctx` is a symptom mask, not a
fix.**

## 5. Corpus sweep — 107 committed fixtures, before and after

Every `crates/ec-av1/fixtures/*.obu`, decode-order dumps vs the oracle
(`EC_AV1_DECODE_ORDER_DUMP` / `EC_AV1_FINAL_DUMP`):

| | base `6bb66a4a` | with the fix |
|---|---:|---:|
| EXACT | 64 | **65** |
| RED (decodes, some frame differs) | 34 | **33** |
| FRAMECOUNT mismatch | 9 | 9 |
| REFUSED | 0 | 0 |

Per-fixture diff: **one row changes.**

```text
420_mixll_altref_256x128_5f   RED (f1..f5)  ->  EXACT
```

No fixture goes EXACT → RED, none gains a refusal, none changes its frame count.
The change is a strict improvement over the committed corpus.

## 6. Class sweep — *a per-segment flag consulted as a branch alternative
instead of a conjunct of the condition it gates*

Every `lossless(fctx)` consult in `decode.rs` re-derived against libaom's own
`if` (74 sites). The class needs a **symbol read** whose guard tests the flag as
a branch *alternative*; chroma unit-grid selection is a different concern.

| site | shape | verdict |
|---|---|---|
| `12835` | `else if lossless` — chroma unit grid | not this class |
| **`18229`/`18231`** | `if skip \|\| (!lossless && !tx_select) {None} else if lossless {4x4 grid} else {tree}` | **correct** — lossless is the FIRST discriminator, which is what libaom's `!xd->lossless` in the tree condition plus `read_tx_size`'s early return imply |
| `22032`, `23937` | `… && !lossless(fctx)` in the tree condition | conjunct — correct |
| `24642`, `24716`, `25762`, `42459`, `44261`, `48632` | `else if lossless` — chroma unit grid (4:2:2 / 4:4:4 / 4:2:0 rect chroma) | not this class |
| **`27213`/`27236`** | intrabc 4x8/8x4 rect leaf | **WAS this class — fixed** |
| `27572` | `tx_select && !lossless` | conjunct — correct |
| `29267` (`read_tx_size`), `29790` (`read_block_tx_size`), `30315` (`read_block_tx_size_rect`) | `if lossless { return / 4x4 }` before the tree | early return — correct |
| `14087`, `15549`, `16188`, `16754`, `18765`, `19742`, `47264` | `tx_select && !lossless` | conjunct — correct |
| **`48332`/`48348`** | sub-8 4x8/8x4 inter leaf | **WAS this class — fixed** |
| `476`, `485`, `12234`, `14617`, `15098`, `15307`, `15320`, `18342`, `18421`, `18422`, `19108`, `21576`, `21625`, `21664`, `21940`, `23393`, `24147`, `24203`, `24598`, `24850`, `25546`, `25549`, `27374`, `27412`, `29790`, `30315`, `30733`, `30744`, `30815`, `30887`, `37885`, `37894`, `37935`, `42230`, `44043`, `45027`, `45316`, `45769`, `45771`, `47739`, `50236`, `51195`, `51830` | value/units/recon selection, `depth != 0 \|\| lossless`, early returns | not this class |

**Exactly 2 sites were in the class; both are fixed; there is no third.** The
recursive `read_var_tx_size` calls (29750, 29854, 30381, 30449) sit inside the
three functions that carry the lossless early return, verified by reading each
function's first lines.

## 7. The rest of the class's stream sweep

All `testsrc2`, `--passes=1 --cpu-used=4 --end-usage=q --cq-level=0 --aq-mode=1`,
decode-order dumps vs the oracle. "red" = the byte count of each differing frame
(49 152 for 256x128, 115 200 for 176x144/320x240, 230 400 for 352x288,
460 800 for 640x480).

| cell | at base | with the fix |
|---|---|---|
| `m5_256x128` (pin, 5 f) | 6 dumps, red f1..f5: 33 724 / 34 251 / 44 500 / 36 688 / 37 462 | **6/6 EXACT** |
| `m_256x128` (6 f) | 7 dumps, red f1..f6: 36 045 … 43 147 | 7 dumps, red f1..f6: 6 051 / 4 115 / 2 962 / 26 566 / 41 876 / 14 749 |
| `m_176x144` | **REFUSED** (1 dump) | 7 dumps, red f1..f6: 811 / 949 / 630 / 969 / 811 / 24 105 |
| `m_320x240` | 7 dumps, red f1..f6: 67 194 … 97 946 | 7 dumps, **red f6 only: 19 398** |
| `m5_320x240` | 6 dumps, red f1..f5: 85 821 … 99 314 | 6 dumps, red f1..f5: 15 031 / 16 006 / 21 930 / 65 405 / 90 077 |
| `m_352x288` | **REFUSED** (3 dumps) | **7/7 EXACT** |
| `m5_640x480` | 6 dumps, red f1..f5: 313 795 … 422 078 | 4 dumps then REFUSED (§4) |
| `m_640x480` | 7 dumps, red f1..f6: 162 045 … 439 643 | 5 dumps then REFUSED (§4) |
| `m444_8` (4:4:4, 8 f) | 9 dumps, red f1..f8: 79 886 … 92 108 | **9/9 EXACT** |

The 6-frame 256x128 and the 320x240 residuals are **not** claimed closed: they
are separate defects this change shrinks but does not remove.

The 176x144 residual is the `mi=(32,32)` extra-geometry fork already handed to
`lane-av1golomb320`, and this lane re-measured it rather than citing it: paired
`EC_SYMR` on `m_176x144` gives a first divergence at read **25792 both with and
without the conjunct**, and the fix still moves our read count from **41 381 to
105 219** against the oracle's **106 607** — i.e. it closes 63 838 reads of
divergence and leaves 1 388 of that other defect. The two sites are independent,
which is the same conclusion `lanes/av1golomb320.report.md` §4 reached.

## 7b. Composition with `lane-av1offtile` (`e87a8a27`, was `714fb55b`) — measured, then reverted

`714fb55b` (later amended to `e87a8a27`) bounds the lossless rect strip's chroma walk by the chroma PLANE and
closes the `mi=(32,32)` 40-extra-reads fork this lane inherited as 176x144's
residual. Cherry-picked onto this tip for the measurement only (reverted; the branch
carries no trace of it). Their tip is now `e87a8a27`, which differs from the
`714fb55b` I measured in `lanes/av1offtile.report.md` ONLY -- `git diff 714fb55b
e87a8a27 -- crates/` is empty -- so these numbers stand without re-taking
them:

| cell | my clip alone | both clips |
|---|---|---|
| `m5_256x128` (pin) | 6/6 exact | **6/6 exact** |
| `m_176x144` | 7 dumps, red f1..f6 | **7/7 exact** |
| `m_352x288` | 7/7 exact | **7/7 exact** |
| `m444_8` (4:4:4) | 9/9 exact | **9/9 exact** |
| `m5_640x480` | 4/6 dumps then REFUSED, red 64 181 / 98 673 / 63 442 | **identical — the clip changes nothing** |
| `m_640x480` | 5/7 dumps then REFUSED, red 53 685 / 75 891 / 34 386 / 112 145 | identical |

So the two clips compose without overlap and together close every cell of this
recipe at 256x128, 176x144, 352x288 and 4:4:4. The 640x480 residual is
untouched by the composition — the three red byte counts are the same to the
byte — which is the third defect of §4.3, not a chroma-walk defect.

**`lane-av1offtile` originally reported that `mix_640x480_5/6` "still decode with
both applied". That does not reproduce on this tip + their clip** (measured
above); it reproduces on base + their clip, i.e. without the var-tx conjunct.
They re-measured, retracted it verbatim with the re-measurement under it, and
amended to `e87a8a27` (2026-09-30). They also independently re-measured the
"red at base" point on their own tree and got my numbers exactly
(f1 352066, f2 414286, f3 313795, f4 399471 of 460800).

## 8. Mutation proof

The fix surgically reverted from the working tree (the two
` && !lossless(fctx)` suffixes removed from lines 27213 and 48332), the gate
re-run:

```text
running 1 test

thread 'stream::tests::a_420_mixed_lossless_alt_ref_sub8_vartx_witness_is_byte_exact_in_decode_order'
panicked at crates/ec-av1/src/stream.rs:10935:17:
a_420_mixed_lossless_alt_ref_sub8_vartx_witness_is_byte_exact_in_decode_order:
decode-order frame 1 of 6 (5 shown, 1 hidden) differs from the oracle at byte 33
(ours 82 vs 81), 33724 bytes differ
test stream::tests::a_420_mixed_lossless_alt_ref_sub8_vartx_witness_... ... FAILED

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 803 filtered out
```

**33 724 -- the same number as the hand-measured red in section 1 / section 7.**
Restored, the same gate is green and prints its non-vacuity evidence:

```text
a_420_mixed_lossless_alt_ref_sub8_vartx_witness_is_byte_exact_in_decode_order:
6 decode-order frame(s) byte-exact (1 hidden), 5 shown frame(s) exact per plane,
40 sub-8 leaf/leaves on a lossless segment read no var-tx symbol
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 803 filtered out
```


## 9. Scoped test command and output

```text
CARGO_TARGET_DIR=$HOME/.cache/cargo-target-av1mixllconj EC_NOMEMGUARD=1 \
  cargo test -p ec-av1 --lib -- 420 444 422 lossless stream \
  --skip bitrate_target_lands_within_5_percent_over_48_frames
```

```text
test result: ok. 375 passed; 0 failed; 7 ignored; 0 measured; 422 filtered out;
             finished in 1770.01s
```

**375 / 0 / 7 / 422.** `lanes/av1mixllfork.report.md` §8 records
`373 / 0 / 7 / 422` for the same scope on its (older) base; the two extra
passes are this lane's new gate plus the `4:4:4` chroma palette gate that landed
on `main` after that report, so the numbers are comparable and the run is a
real-delta result, not a no-delta confirmation.

**The scope limit this run cannot cover, restated because it is load-bearing:**
it contains **no 640x480-class cell**. The corpus sweep in §5 covers the
committed fixtures and the cell sweep in §7 covers the generated ones, so the
§4 refusal is caught by neither the suite nor the corpus -- it needs its own
generated cell, exactly as the prior lane warned.

## 10. What is NOT measured

* **The 640x480 cell does not decode with this fix in.** §4 names the blocker
  (a missing `TXFM_CONTEXT` band write above `mi=(80,110)`, class
  `context-band-not-published`) and reproduces its effect in one line, but the
  band write itself is not identified to a call site — the replay only shows
  that libaom has a write in rows 37..79 at mi column 110 and this tree has
  none. That is the next lane's job, and it is NOT this lane's hunks.
* **10/12-bit arm of the class sweep: not synthesized.** This ffmpeg cannot emit
  a >8-bit y4m and a hand-built 16-bit-LE one is rejected by the shared
  `aomenc` (`Loss of framing in Y4M input data`). The committed HBD corpus is
  covered by the §5 regression; that is a regression, not a sweep of this class
  at HBD.
* The class is content-dependent: everything above is `testsrc2` at five sizes.
  The 106 committed fixtures that do not come from this recipe are the
  regression evidence (§5), not a sweep of the class.
* The forced-`ctx` override in §4.3 is a **symptom mask used as a proof**, not a
  candidate fix; it is not in the tree.

## 11. Fix-now | deferred(<unblock>) | accepted

* **fix-now** — the two-line conjunct, its fixture, its route counter and its
  gate. Landed in this commit.
* **deferred(unblock: identify the `TXFM_CONTEXT` band write libaom makes above
  `mi=(80,110)` on a 640x480 mixed-lossless cell, rows 37..79, mi column 110 —
  the last write this tree makes is a skipped 16-px inter block at `mi=(36,108)`
  publishing 16 where libaom has 4)** — that is the whole 640x480 cell, and the
  one regression control this lane does not meet. **Owner: `Kerem-8`, lane
  `lane/av1txctxband2`**, which has claimed the band-write family; §4.3 and the
  joint reproduction in `lanes/av1offtile.report.md` §6b hand it the numbers, the
  per-site override and the `EC_TXUPD` replay, so the chain need not run through
  this lane.
* **deferred(identification of the residual reds on the 6-frame 256x128,
  320x240 and 176x144 arms)** — the 176x144 one is `lane-av1golomb320`'s
  `mi=(32,32)` fork; the other two are unnamed and this lane does not claim
  them.
* **deferred(a >8-bit y4m source, or a hand-built one the shared `aomenc`
  accepts)** — the HBD arm of the class sweep, §10.
* **accepted** — the `EC_MIXLL` per-frame table print, the `EC_SITETAG` /
  `EC_TXCTX_SITE` / `EC_TXUPD` diagnostics and the `set_symr_cdf` tags are all
  **reverted**; the tree carries no probe bypass and no env-gated trace. The
  permanent artefacts are this report and the measurements in it. The shared
  oracle was never rebuilt or edited: the one oracle rung this lane needed
  (`EC_TXCTXB`) already existed.

## 12. Hunk scope

* Authored here: this report, and nothing else. The `decode.rs` lines are the
  cherry-picked candidate `658d37fa`'s (`27213`, `48332`, plus the
  `SUB8_LOSSLESS_NO_VARTX` counter at `48349`); the fixture and the gate are
  that commit's too, re-measured on this base.
* No line outside those hunks was touched; `git status` in the primary checkout
  is clean.
* Every diagnostic added during this lane was reverted before the commit
  (`git status --porcelain` empty apart from this report).
