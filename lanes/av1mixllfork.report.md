# lane-av1mixllfork — the mix5 fork at read 30889 is LOCALISED to a var-tx **ordering** defect; the candidate fix is **reverted as unproven** (it regresses 640x480 from decode to refusal)

Base `main` = `32d944bf`. Worktree `/home/tahinli/.cache/wt/av1mixllfork`, target dir
`$HOME/.cache/cargo-target-av1mixllfork`, `EC_NOMEMGUARD=1`. Oracle:
`~/.cache/aom-oracle/build/{aomdec,aomenc}` (shared, untouched). ffmpeg 8.1.3.
**This commit adds only this report. `git status --porcelain` is empty apart from
it, the tree is byte-identical to `32d944bf`, no probe bypass and no env-gated
trace is left behind.** The candidate fix was measured, shown to regress a larger
cell, and reverted — §4 has the numbers, §5 says what is still unknown.

---

## 0. Verdict

| | |
|---|---|
| Fork reproduced | **yes** — read **30889** of f1, byte-identical to `lanes/av1framecount.report.md` §5 |
| Fork **shape** corrected | it is **not** a wrong-CDF-row fork (§2) |
| Owning block identified | **yes** — a sub-8 4x8/8x4 inter leaf's var-tx gate, `decode.rs:48259`; second site `decode.rs:27140` |
| Root cause named | libaom puts `!xd->lossless[mbmi->segment_id]` **in** the var-tx condition; ours put `lossless(fctx)` in the `else if` |
| Candidate fix | 2 lines; **green on 4 cells, REGRESSION on 2** → **reverted, unproven** |
| Regression | 640x480 4:2:0 8-bit mixed-lossless+altref goes **decode → `REFUSED: AV1 tile (a Golomb tail longer than this decoder reads)`** |
| Fixture / gate | **none committed** — a gate that asserts a cell the tree still fails is not landable |
| Class sweep | done on the **source** (9 var-tx call sites, 2 broken) and on **streams at base**; HBD not synthesized (§7) |

## 1. Reproduction

The encoder side reproduces **byte-exact** on the first try:

```text
ffmpeg -f lavfi -i testsrc2=size=256x128:rate=25 -frames:v 5 -pix_fmt yuv420p src5b.y4m
aomenc --codec=av1 --obu -o c_mix5.obu --passes=1 --cpu-used=4 \
       --limit=5 --end-usage=q --cq-level=0 --aq-mode=1 src5b.y4m
```

`c_mix5.obu` = **18 525 B**, sha256
**`3e06b5641e6a0f5a3f4ac8f114d4ee48c88638429e9eaafe6b4483e24f1dbc3a`** — the
sha the inherited report names. ffmpeg, rebuilt 2026-09-30, still picks the same
AQ decisions. Decode-order dumps vs the oracle at `32d944bf`:

| frame | diff bytes (of 49 152) |
|---|---:|
| f0 (key) | 0 |
| f1 | **33 724** |
| f2 | **34 251** |
| f3 | **44 500** |
| f4 | **36 688** |
| f5 | **37 462** |

## 2. Two corrections to the inherited frame

### 2a. "The oracle makes no block origin at mi=(6,1)" is false

Claimed by `lanes/av1framecount.report.md` §5 and relayed by the parent. The
oracle's own `EC_ECDUMP_IN` rung says otherwise:

```text
EC_ECDUMP_IN plane=0 mi=(6,0) bc=0 br=0 tx=0 ctx=1 ECIN=(55806,57232,2608)
EC_ECDUMP_IN plane=0 mi=(6,0) bc=0 br=1 tx=0 ctx=3 ECIN=(8271,57096,2646)
EC_ECDUMP_IN plane=0 mi=(6,1) bc=0 br=0 tx=0 ctx=3 ECIN=(35995,39325,2653)   <- the fork
EC_ECDUMP_IN plane=0 mi=(6,1) bc=0 br=1 tx=0 ctx=3 ECIN=(5855,55048,2690)
EC_ECDUMP_IN plane=1 mi=(6,1) bc=0 br=0 tx=0 ctx=8 ECIN=(11710,48168,2691)
EC_ECDUMP_IN plane=2 mi=(6,1) bc=0 br=0 tx=0 ctx=8 ECIN=(11129,49928,2749)
```

The oracle has **two** TX_4X4 luma units at `mi=(6,1)` (`br=0`, `br=1`) plus one
chroma unit per plane — a 4x8 luma transform on a sub-8 leaf, the **lossless**
shape. It is **our** decoder that lacks that geometry. The "open geometry
question" is therefore not open: it is a *consequence* of the missing lossless
branch, and the sibling observation (odd-column origins appear in 7 other rows)
stays true.

### 2b. It is not a wrong-`txs_ctx` / wrong-`txb_skip_ctx` fork

The inherited report reads the `cdf0` difference as a context bug. Measured, the
`32768 - x` ICDF mirror between the two sides' `cdf0` **holds at every agreeing
read**, including the three immediately before the fork (`8580↔24188`,
`30886↔1882`, `16138↔1882`'s neighbour `16138↔16630`), and **breaks exactly at
the fork** (`15299` vs `22401`; `32768 - 15299 = 17469 ≠ 22401`).

So the two sides are reading **different symbols**, not one symbol out of a
different row. A wrong `txs_ctx` or `txb_skip_ctx` can never produce that. The
read is one step out of sync with a symbol that libaom does not read at all.

**How our read was identified** (this is the part that is easy to get wrong).
Our `EC_SYMR` prints no site, and `ph=` is **sticky** — it names whichever reader
last called `set_symr_phase`, so a coefficient read shows the enclosing block's
phase and a sub-8 read that never sets one inherits an older value. `mi=` is
likewise stale. I tagged every `intra_inter` / `new_mv` / `drl_mode` / `zero_mv` /
`ref_mv` / `obmc` / `switchable_interp` read with `set_symr_cdf` in **all four**
inter-mode-info copies, rebuilt, and re-ran:

```text
read 30888  cdf=refmvB  pre=(49935,53265,2668) cdf0=24188 n=2 s=0 post_rng=39325
read 30889  cdf=        pre=(35995,39325,2668) cdf0=22401 n=2 s=0 post_rng=54010
```

`refmvB` fired — the block is a sub-8 piece (`decode.rs:48185`) and `s=0` is
NEARESTMV on **both** sides (same `ref_mv` row, mirrored). Read 30889 carries
**no** tag, so it is none of the six inter mode-info reads; the one read a block
makes between its `ref_mv` and its coefficients is the var-tx tree's
`tx_size_cat1`. (Our `EC_ECDUMP_IN` also fires for every `read_coeffs` /
`read_coeffs_rect` call, and the fork's entry state `(35995, 39325, …)` is
absent from it — so the read is not a `txb_skip` at all.) The tags were
**reverted**; they are a finding, not an artefact.

Oracle at the same index:

```text
read 30888  site=decodemv.c:191 mi=(6,1) pre=(49935,53265,2653) cdf0=8580  n=2 s=0 post_rng=39325
read 30889  site=decodetxb.c:158 mi=(6,1) pre=(35995,39325,2653) cdf0=15299 n=2 s=0 post_rng=42076
read 30890  site=decodetxb.c:242 mi=(6,1) ... n=5
```

libaom goes straight from `refmv` to the luma `txb_skip`. **It reads no
`tx_size` symbol at all.**

## 3. Root cause

`decodeframe.c:1237`, `parse_decode_block`:

```c
if (cm->features.tx_mode == TX_MODE_SELECT && block_signals_txsize(bsize) &&
    !mbmi->skip_txfm && inter_block_tx && !xd->lossless[mbmi->segment_id]) {
  ... read_tx_size_vartx ...
} else {
  mbmi->tx_size = read_tx_size(xd, ...);      // decodeframe.c:1208:
}                                            //   if (xd->lossless[seg]) return TX_4X4;
```

`!xd->lossless[mbmi->segment_id]` is a **conjunct of the tree condition**, and
`read_tx_size` answers `TX_4X4` on the same flag. A lossless *segment* of a
mixed-lossless frame codes no `tx_size_cat1` symbol and lands on TX_4X4 per
plane.

Ours, at `32d944bf` (`decode.rs:48231-48247`, the sub-8 4x8/8x4 inter leaf):

```rust
if fctx.tx_select_inter.with(Cell::get) && !skip {   // no lossless conjunct
    read_var_tx_size(..)                             // reads tx_size_cat1
} else if lossless(fctx) && !skip {                  // one branch too late
    for row in 0..bh / MI { for col in 0..bw / MI { leaves.push((row, col, MI, MI)); } }
```

`lossless(fctx)` is already per-segment (`decode.rs:453-456`,
`lossless_per_seg[cur_segment_id]`), so it answers correctly — one branch too
late. Everything the tree reads after the spurious `tx_size_cat1` is then offset
by one symbol and the **next** block's `txb_skip` is the first visible fork.
That is exactly the reported signature: a correct pre-state, the same alphabet,
a different result.

**Defect class, named:** *a per-segment flag consulted as a branch alternative
instead of as a conjunct of the condition it gates.*

## 4. The candidate fix, and why it is reverted

```diff
@@ decode.rs:27140 (intrabc 4x8 / 8x4 rect leaf)
-            if tx_select && !skip {
+            if tx_select && !skip && !lossless(fctx) {
@@ decode.rs:48259 (sub-8 4x8 / 8x4 inter leaf)
-        if fctx.tx_select_inter.with(Cell::get) && !skip {
+        if fctx.tx_select_inter.with(Cell::get) && !skip && !lossless(fctx) {
```

plus a `SUB8_LOSSLESS_NO_VARTX` route counter for non-vacuity. It was committed
as `658d37fa` and **then reverted** (`git reset --hard 32d944bf`).

**Green** (measured on `658d37fa`):

| cell | at base | with the fix |
|---|---|---|
| `c_mix5` 256x128 `--limit=5` (the pin) | RED f1–f5 | **6/6 decode-order byte-exact**, 5/5 shown byte-exact, **96 997/96 997 entropy reads bit-identical, zero divergence over the whole stream** |
| `s_arf8` 256x128 `--limit=8` | RED f1–f5 | **6/6** |
| `s444_8` **4:4:4** `--profile=1 --limit=8` | **RED f1–f8** | **9/9** |
| `s176` 176x144 `--limit=6` | `REFUSED` | 7/7 dumps, refusal removed (still red — see §6) |
| `s352` 352x288 `--limit=6` | `REFUSED` | 7/7 **exact** (per lane-av1golomb320) |

Mutation proof on the pinned fixture, fix surgically reverted from the working
tree, gate run:

```text
test stream::tests::a_420_mixed_lossless_alt_ref_sub8_vartx_witness_... ... FAILED
panicked at crates/ec-av1/src/stream.rs:10935:17:
...: decode-order frame 1 of 6 (5 shown, 1 hidden) differs from the oracle at
byte 33 (ours 82 vs 81), 33724 bytes differ
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 802 filtered out
```

**33 724 — the same number as the hand-measured red.**

**Regression** (found by lane-av1golomb320, reproduced and confirmed here):

```text
cell: testsrc2 640x480, 6 frames, --passes=1 --cpu-used=4 --limit=6 \
      --end-usage=q --cq-level=0 --aq-mode=1   -> 115615 B

  32d944bf (base)  : OK: 6 frames decoded, 640x480
  658d37fa (fix)   : REFUSED: unsupported: AV1 tile (a Golomb tail longer than this decoder reads)
```

`--limit=5` (87 537 B) behaves identically (base decodes 5, fix refuses). I
isolated which of the two hunks does it: reverting **only** `27140` and keeping
`48259` still refuses, so the regression comes from the **sub-8 inter** hunk.
Locating it, the first divergence on `m640_6` with the fix is at read **184 328**,
where the oracle reads `decodeframe.c:1186` — the `txfm_partition` split inside
`read_tx_size_vartx` — and we read a different `n=2` symbol from the identical
pre-state `(30248, 45616)`:

```text
ORC 184328 decodeframe.c:1186 mi=(91,106) pre=(30248,45616) n=2 s=0 post_rng=62032
OUR 184328  (untagged)         mi=(88,108) pre=(30248,45616) n=2 s=0 post_rng=58828
```

and the oracle's `EC_ECDUMP_IN` for that block is a single `tx=0` (TX_4X4) luma
unit at `mi=(91,106)`. So at that leaf the oracle **does** walk the var-tx tree
and does **not** consider its segment lossless, while our `lossless(fctx)`
returned true and suppressed the tree. Instrumenting the gate
(`EC_MIXLLDBG`, since removed) shows the flag is genuinely reporting `true` on
many sub-8 leaves of this stream (145 of 160 leaves), with `cur_segment_id`
values 0, 1, 4 and 5 in play.

**Conclusion:** the ordering defect is real and the fix is right *at the leaf*,
but our per-segment lossless answer is **wrong somewhere in the 640x480 stream**
— over-reporting on a block whose segment libaom treats as lossy. Adding the
conjunct therefore converts one wrong-symbol fork into a *missing*-symbol fork on
that cell. Per the lane contract ("a change you cannot make red first is not a
fix — report it as unproven instead") and because a decode → refusal flip is a
refusal-inventory regression in its own right, the change is **reverted and no
fixture or gate is committed**: a gate asserting a cell the tree still fails is
not landable.

## 5. Class sweep (on the reverted tree, i.e. at `32d944bf`)

**(a) Source — every var-tx gate, re-derived against libaom's own `if`.**
`read_var_tx_size` has 9 call sites:

| site | shape | verdict |
|---|---|---|
| `decode.rs:18229` | `if skip \|\| (!lossless && !tx_select) … else if lossless` | lossless first — **correct** |
| `decode.rs:21973` | `… && tx_select && !skip && !lossless(fctx)` | conjunct — **correct** |
| `decode.rs:23868` | `… && !skip && !lossless(fctx)` | conjunct — **correct** |
| `decode.rs:14042`, `15504`, `16143`, `16709`, `18720`, `19697` | `tx_select && !lossless(fctx)` | conjunct — **correct** |
| `decode.rs:29708` (`read_block_tx_size`) | `if lossless(fctx) { … return }` before the tree | early return — **correct** |
| `decode.rs:30235` (`read_block_tx_size_rect`) | same early return | early return — **correct** |
| **`decode.rs:27140`** (intrabc 4x8/8x4 rect) | `tx_select && !skip` … `else if lossless && !skip` | **this class** |
| **`decode.rs:48231`** (sub-8 4x8/8x4 inter) | `tx_select_inter && !skip` … `else if lossless && !skip` | **this class** |

The other six `else if lossless(fctx)` sites (`12790`, `25689`, `42365`,
`44172`, `48549`, `18186`) are **chroma unit-grid** selection, not var-tx symbol
reads — a different concern, not this class. **Exactly 2 sites.**

**(b) Streams — at base, so the numbers are the tree's real state.** All arms
`testsrc2`, `--passes=1 --cpu-used=4 --end-usage=q`, decode-order dumps vs the
oracle:

| cell | flags | base |
|---|---|---|
| `c_mix5` | `--limit=5 --cq-level=0 --aq-mode=1` | RED f1–f5 |
| `s_arf8` | `--limit=8 --cq-level=0 --aq-mode=1` | RED f1–f5 |
| `s444_8` | `--profile=1` (4:4:4) `--limit=8 --cq-level=0 --aq-mode=1` | RED f1–f8 |
| `s_noarf5` | `--auto-alt-ref=0 --cq-level=0 --aq-mode=1` | 5/5 exact |
| `s_cq20` | `--cq-level=20 --aq-mode=1` | 6/6 exact |
| `s_ll` | `--lossless=1` | 6/6 exact |
| `s_aq0` | `--cq-level=0 --aq-mode=0` | 6/6 exact |
| `s176` | 176x144 `--limit=6` | **REFUSED** |
| `m640_5` / `m640_6` | 640x480 `--limit=5` / `--limit=6` | decodes (redness not measured) |

The four exact arms confirm the trigger really is the conjunction (per-segment
mixed lossless **and** an alt-ref structure) the inherited report described.

## 6. Not this lane's defect

* **Aras-3's `mi=(32,32)` fork at read 25792** on the 176x144 cell is a
  **separate** extra-geometry defect. Measured here on both sides of my change:
  the first divergence is **25792 with and without the fix**, so the two-line
  ordering change does not move it at all. Handed to lane-av1golomb320.
* The 320x240 Golomb-tail refusal of the same recipe was not reproduced here.

## 7. What is NOT measured

* **The next step is not taken, and it is the whole remaining question:** *why
  does `lossless(fctx)` return true on a 640x480 sub-8 leaf whose segment libaom
  treats as lossy?* Two candidates, neither tested: `fctx.cur_segment_id` is
  stale at some sub-8 leaf (`inter_segment_id` is called per piece at
  `decode.rs:48026` / `46262`, and the sub-8 reader has its own skip-then-
  segment order), or `lossless_per_seg` itself is wrong for some segment. The
  instrumentation that would settle it is one env-gated line at
  `decode.rs:48231` printing `(rmi, cmi, cur_segment_id, lossless)`, paired with
  the oracle's `EC_ECDUMP_IN` for the same `mi`. **I ran that instrumentation and
  it shows the flag is true (145/160 leaves) but I did not finish the pairing,
  so I cannot say which candidate it is.**
* **10/12-bit arms: NOT synthesized.** This ffmpeg (8.1.3) cannot emit a
  >8-bit y4m at all — `Conversion failed!` for `yuv420p10le`, `yuv420p12le`,
  `yuv444p10le` and `gray10le`, from both a lavfi source and a y4m input — and a
  hand-built 16-bit-LE y4m is rejected by the shared `aomenc` with
  `Loss of framing in Y4M input data` (0-byte output). The committed HBD corpus
  is covered by the scoped regression, but that is a **regression, not a sweep of
  this class at HBD**, and I do not claim it as one.
* Whether the 640x480 base decode is red or exact — I measured that it *decodes*,
  not that it is correct.
* `mixll` is content-dependent. I re-measured only `testsrc2` at 256x128, plus
  the 4:4:4 256x128, 176x144, 352x288 and 640x480 arms; I did **not** re-run the
  inherited report's three-generator x three-size matrix.
* Pre-fix, f1 is wrong from pixel 0 at whole-frame scale. I did not bisect which
  reconstruction stage first diverges: with the entropy ladder bit-identical end
  to end under the candidate fix, the parse was the whole story.

## 8. Regression

Exact command, run in the lane worktree on the reverted tree:

```text
CARGO_TARGET_DIR=$HOME/.cache/cargo-target-av1mixllfork EC_NOMEMGUARD=1 \
  cargo test -p ec-av1 --lib -- 420 444 422 lossless stream \
  --skip bitrate_target_lands_within_5_percent_over_48_frames
```

```text
test result: ok. 373 passed; 0 failed; 7 ignored; 0 measured; 422 filtered out;
             finished in 1623.93s
```

**373 / 0 / 7 / 422 — the identical numbers `lanes/av1framecount.report.md` §6
records**, which is the expected result for a report-only change: this tree
carries no source delta from `32d944bf`. It is a no-delta confirmation, not
evidence for any fix, because there is no fix in the tree.

This is the inherited report's scope, so the two are comparable: the 4:2:0,
4:4:4 and 4:2:2 fixtures, the lossless cells, the sub-8 and inter chroma
identity gates, and the whole `stream` module where the oracle-compared fixture
gates live.

**The coverage hole this run exposes.** I ran the same command on the candidate
fix `658d37fa` before reverting it: **236 tests, 0 failures, still running** when
I terminated it — and it never went red, while that commit turns 640x480 from
decoding into a `REFUSED`. **The suite contains no 640x480-class cell**, so a
refusal-inventory regression of exactly this kind is invisible to it. Any lane
that lands a mixed-lossless + altref change needs a >= 640x480 cell of that
recipe in `fixtures/`, or the green run is not evidence.

## 9. Fix-now | deferred(<unblock>) | accepted

* **fix-now** — nothing in the tree. The change is reverted; this commit is the
  report alone.
* **deferred(identify whether `fctx.cur_segment_id` is stale at the sub-8 leaf,
  or `lossless_per_seg` is wrong, on a 640x480 mixed-lossless+altref cell)** —
  the whole remaining fix. §7 names the one env-gated line that settles it and
  the oracle rung to pair it with. Until that is answered, adding
  `&& !lossless(fctx)` trades one fork for another.
* **deferred(a >8-bit y4m source, or a hand-built one the shared `aomenc`
  accepts)** — the HBD arm of the class sweep, §7.
* **deferred(a separate lane)** — the `mi=(32,32)` extra-geometry fork at read
  25792 and the 320x240 Golomb-tail refusal. §6.
* **accepted** — the diagnostic `set_symr_cdf` tags and the `EC_MIXLLDBG` print.
  Both identified the reads and both are reverted; the permanent artefacts are
  this report and the measurements in it. If a future lane needs to name an
  untagged inter read again: `refmvB`-style tags at all **four**
  inter-mode-info copies are the lines to re-add, and `ph=`/`mi=` must not be
  trusted to do it.
