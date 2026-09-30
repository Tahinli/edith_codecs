# lane-av1offtile — the 40 off-tile reads are `decode_rect_split`'s LOSSLESS chroma walk: it counted the block's own `chroma_w/4 x chroma_h/4`, libaom counts the CHROMA PLANE's

Base `main` = `6bb66a4a`. Worktree `/home/tahinli/.cache/wt/av1offtile`, target dir
`$HOME/.cache/cargo-target-av1offtile`, `EC_NOMEMGUARD=1`. Oracle
`~/.cache/aom-oracle/build/{aomdec,aomenc}` (shared, untouched). ffmpeg 8.1.x.
**No var-tx conjunct taken, no edit to `decode.rs:48259` / `:27140` (Emre-5 owns
those), no probe bypass and no env-gated trace left in the tree.**

---

## 0. Verdict

| | |
|---|---|
| Owner of the 40 reads identified | **yes** — `fn decode_rect_split`, the `else if lossless(fctx)` arm's plane-major 4x4 chroma double loop (`for plane_idx in 1..=2 { for cu_row in 0..ch_n { for cu_col in 0..cw_n }`), fed by `let (cw_n, ch_n) = (chroma_w / 4, chroma_h / 4)` |
| Is it the tile loop? | **no.** The tile walk (`decode_tile`'s `sb_visit_order` loop, `decode.rs` ~35086) sets `set_tile_origin` correctly and is not implicated; §4 measures this |
| Extent walked | the **block's own** `chroma_w / 4` x `chroma_h / 4`. libaom walks `max_block_wide/high(xd, plane_bsize, 1)`, i.e. the **chroma plane's** extent |
| Why the bit position freezes | the walk ran **off the end of the tile's byte range** into an exhausted `SymbolDecoder`; `dec.literal(1)` then returns 0 forever, so every further `aom_read_symbol` renormalises `rng` (decaying ~509/read here) and advances **no** bits |
| Fix | **landed** — 2 lines, the chroma twin of the luma `max_blocks_wide/high` clip already in the same function |
| 40 extra reads | **gone**; frame 0's transform-unit count is now the oracle's own number (2373 = 2373) |
| The hard refusal | **still present on this cell at this commit**, and it is NOT this walk's — §5 measures exactly which defect now owns it |
| 640x480 control | **decodes 5/5 and 6/6 unchanged, and RED on pixels at base** — this walk is inert there (§6) |
| Gate + fixture + mutation proof | **yes**, §6 |
| A wrong claim, caught and corrected | **yes** — §5, 640x480 under the COMBINED fixes |
| Class sweep | **done**, §7: one site; the two sibling chroma walks never trip the same condition on 18 generated edge-overhang cells or the whole committed corpus |

## 1. The cells

Same recipe as the inherited context, per size:

```text
ffmpeg -f lavfi -i testsrc2=size=<S>:rate=25 -frames:v 6 -pix_fmt yuv420p s.y4m
aomenc --codec=av1 --obu -o <out> --passes=1 --cpu-used=4 --limit=6 \
       --end-usage=q --cq-level=0 --aq-mode=1 s.y4m
```

| cell | bytes | sha256 |
|---|---:|---|
| `mix_176x144_6.obu` | 20 069 | `02b15d0d5b08aa5aebf139aee94c31b4f8363d14649c238cd7a19a9785ef124a` |
| `mix_352x288_6.obu` | 46 490 | `e43c74b8d847e701953cef19dbfb78e4efb5949c4c9293a74201b075a0c85d81` |

At `6bb66a4a` both `REFUSED: unsupported: AV1 tile (a Golomb tail longer than
this decoder reads)`, reproducing the inherited frame exactly. Controls at the
same base, all decoding: `mix_640x480_5/6`, `mix_320x240_6`, `mix_176x144_5`,
`c_mix5.obu` (the peer's 256x128 red cell).

## 2. The 40 reads, named

Paired `EC_SYMR` (`EC_SYMR=1` both sides), compared on `pre[0]`, `pre[1]`, `n`,
`s`, `post_rng`; `pre[2]` and `cdf0` carry the known per-site constant offset
and the ICDF-mirroring convention.

```text
reads 0..25791    IDENTICAL on both sides
read  25791       both: mi=(32,32) txb_skip, pre=(15655,42168,·) n=2 s=1 post_rng=41660
read  25792       ORACLE: decodeframe.c:1348  pre=(4399,32768,0)   <- range-coder init: frame 0 ENDED
read  25792       OURS:   mi=(32,32) txb_skip pre=(15655,41660,·) n=2 s=1 post_rng=41152
...  reads 25792..25831  OURS ONLY, 40 extra
```

Ours total 41 381 reads, the oracle 106 820. With the +40 offset the two sides
re-lock for **3 619 reads** (`oracle[k-40] == ours[k]` on all five compared
fields), then fork at oracle 29 411 / ours 29 451.

**The owning walk, named.** Every one of the 40 is `tag=all_zero side=4` — the
plane-major 4x4 chroma unit read of `fn decode_rect_split`'s
`else if lossless(fctx)` arm. Env-gated `EC_CENSUS_UNIT` gives the plane split
directly, and `EC_ECDUMP_IN` gives the per-unit geometry:

```text
frame 0, our last block:  48 units plane 0 | 32 units plane 1 | 32 units plane 2
frame 0, oracle mi=(32,32): plane 0 bc 0..11 br 0..3 (48 units)
                           plane 1 bc  0..5 br 0..1 (12 units)
                           plane 2 bc  0..5 br 0..1 (12 units)      = 72 total
```

**48 - 12 = 20 phantom units per plane, x2 planes = 40.** The arithmetic is
exact, not fitted.

**Which block, and how big.** A temporary env-gated rung inside
`decode_rect_split` (added, measured, removed — not in the tree) printed the
walk's own inputs:

```text
EC_OFFTILE rectsplit mi=(32,32) px=(128,128) bw=64 bh=32 cw=32 ch=16 cpx=64 cpy=64
                tx=4x4 tw=176 th=144 utw=88 uth=72
```

`176x144` frame, `mi=(32,32)` = pixel (128,128): the bottom-right superblock is
the **48x16 sliver** the brief names, but the block coded there is
`bw=64 bh=32` (a partition size that overhangs), so its chroma plane block is
`cw=32 ch=16` at origin `cpx=64 cpy=64` — while the chroma plane is only
**88x72**. The walk therefore counted `cw_n = 32/4 = 8`, `ch_n = 16/4 = 4` and
emitted 8x4 units per plane where only **6x2** exist.

## 3. What libaom does instead, and why the bit freezes

`av1_common_int.h:1565`:

```c
static inline int max_block_wide(const MACROBLOCKD *xd, BLOCK_SIZE bsize, int plane) {
  int max_blocks_wide = block_size_wide[bsize];
  if (xd->mb_to_right_edge < 0) {
    const struct macroblockd_plane *const pd = &xd->plane[plane];
    max_blocks_wide += xd->mb_to_right_edge >> (3 + pd->subsampling_x);
  }
  return max_blocks_wide >> MI_SIZE_LOG2;
}
```

Two things matter and both were missing:

1. **It is per PLANE.** `pd->subsampling_x` is the PLANE's subsampling, so the
   bound is in chroma pixels. Ours had no chroma-plane bound at all.
2. **It is applied per unit.** `av1_foreach_transformed_block_in_plane`
   (`decodeframe.c:1124-1125`) drops the unit:
   `if (blk_row >= max_blocks_high || blk_col >= max_blocks_wide) return;`

`decode_rect_split`'s **luma** walk already carries that clip — the
`tu_px >= y.true_width || tu_py >= y.true_height` guard lane-hgkf r1 added — and
it is why our 48 luma units are already correct. The **lossless chroma** walk
two hundred lines below it did not, which is the whole defect: the same
function, the same rule, one plane covered.

**Why the bit position freezes.** The 40 units are beyond the tile's coded
extent. `read_golomb` (`decode.rs:7989-8018`) counts its unary prefix with
`dec.literal(1)`, and an exhausted `SymbolDecoder` returns 0 forever, so the
tail runs past the `length > 20` cap and the walk desyncs out of a tile with no
data left. Measured on the frozen region: `bit == post_bit == 36288` on all 40,
`range` decaying ~509 per read (`41660 -> 41152 -> 40644 -> …`). The oracle's own
frozen-region reads (`bit == post_bit == 19908`, 62 of them) are the same
phenomenon at the tail of the last real block — the difference is **how many**
phantom units each side emits after its own last real one.

## 4. The fix

```rust
// decode_rect_split, else-if lossless(fctx) chroma arm, inside the cu_row/cu_col loop
if cpx + cu_col * 4 >= u.true_width || cpy + cu_row * 4 >= u.true_height {
    hit!(RECT_SPLIT_LOSSLESS_CHROMA_OFFTILE_HITS);
    continue;
}
```

the exact chroma twin of the luma guard already 380 lines above it in the same
function, using the chroma plane's own `true_width`/`true_height` (88x72 here)
exactly as libaom's `max_block_wide(xd, plane_bsize, /*plane=*/1)` does.

**It is not the tile loop.** `decode_tile`'s `sb_visit_order` walk sets
`set_tile_origin` per tile correctly and never appears in the divergence: the
phantom units are stamped `mi=(32,32)`, which is inside the tile, and the tile's
own `mi_col1 * 4` bound is what `sb_r1`/`sb_c1` already derive. The block
footprint, not the tile, was the wrong extent. Measured, not inferred: with the
clip in place the tile walk's `sb_c1`/`sb_r1` are unchanged and the ladder
divergence simply moves.

**Result on the paired ladder:**

```text
                       before                    after
first mismatch read    25792                     29411
reads 0..N identical   0..25791                  0..29410   (frame 0 FULLY bit-identical)
extra all_zero reads   40 (ours 25792..25831)    0
```

Frame 0 now consumes **2373** transform units against the oracle's own
`EC_ECDUMP_IN` census of **2373** for the same bytes (base: 2413).

## 5. The refusal: what this commit does and does not fix

Stated plainly because it is the acceptance question.

* **This walk is fixed and the 40 reads are accounted for.** Frame 0 is
  bit-identical to the oracle for all 25 792 reads and its unit census matches.
* **The cell still refuses at this commit**, because a **second, independent**
  defect now owns the first divergence — read 29411, oracle `mi=(9,14)`
  `decodemv.c:1036 -> decodetxb.c:158`, ours `mi=(8,12)` reading a different
  CDF row (`cdf0` 20675 vs 22401) off the **identical** pre-state
  `(41839, 52168)`. That is the var-tx/`lossless`-conjunct class, i.e. the
  `decode.rs:48259` / `:27140` gates **Emre-5 owns**, and it is out of my scope.
* **The two fixes compose and close the cell.** Measured in a scratch worktree at
  Can-3's `658d37fa` with this patch applied on top:

  ```text
  mix_176x144_6.obu  OK: 6 frames decoded, 176x144   (base: REFUSED)
  mix_352x288_6.obu  OK: 6 frames decoded, 352x288   (base: REFUSED)
  both, byte-for-byte against aomdec --rawvideo:   cmp: IDENTICAL
    228096 bytes (176x144) and 912384 bytes (352x288)
  ```

  **CORRECTION (after Emre-5's counter-measurement).** An earlier draft of this
  report claimed that both fixes applied together leave `mix_640x480_5/6`
  decoding. **That was wrong — I had measured 640x480 only against MY tree, not
  against the combination, and extrapolated.** Re-measured on the combined
  scratch binary (`658d37fa` + this patch):

  ```text
  mix_640x480_5.obu  REFUSED: unsupported: AV1 tile (a Golomb tail longer than this decoder reads)
  mix_640x480_6.obu  REFUSED: unsupported: AV1 tile (a Golomb tail longer than this decoder reads)
  ```

  Emre-5 measured the same and, further, that my clip changes nothing there —
  the per-frame red byte counts are identical with and without it. This
  patch touches no var-tx gate, so that is the expected answer; it just is not
  the answer I had claimed. The 640x480 refusal with both applied is Emre-5's
  §5 regression, unowned by this lane.

* **I do not claim this commit removes the refusal on its own.** It removes the
  off-tile walk; the remaining refusal belongs to Emre-5's lane, and I did not
  take that route as instructed.

## 6. Regression control, gate, fixture, mutation

**Regression controls — all at this commit, `decode_probe`:**

"Decodes" below means *not refused* — NOT that the pixels are right. It is stated
that way because on 640x480 the two are very different facts, and conflating
them is what produced the wrong claim in §5.

| cell | at base `6bb66a4a` | with this fix | pixels? |
|---|---|---|---|
| `mix_640x480_5.obu` | decodes 5/5 | **decodes 5/5, unchanged** | **RED on every inter frame** — see below |
| `mix_640x480_6.obu` | decodes 6/6 | **decodes 6/6, unchanged** | red, same class |
| `mix_320x240_6.obu` | decodes 6/6 | **decodes 6/6** | not measured by this lane |
| `mix_176x144_5.obu` | decodes 5/5 | **decodes 5/5** | not measured by this lane |
| `c_mix5.obu` (peer's 256x128 red cell) | decodes, red on pixels | **untouched by this patch** | red at base |

**640x480 does not decode CORRECTLY at base, and saying it "decodes" without
that caveat is misleading.** Measured here on my own tree (decode-order dumps vs
the oracle's `EC_AV1_FINAL_DUMP`, `mix_640x480_5.obu`, 460 800 B/frame):

```text
f0 EXACT
f1 352066 bytes differ
f2 414286
f3 313795
f4 399471
```

i.e. roughly 80 % of every inter frame is already wrong before any change. A
refusal there replaces silent corruption with a loud failure; it does not
remove a working decode. Emre-5 measured the identical counts, and also that
they are **unchanged by this patch** — which is the point: this walk is inert
on that cell.

**Fixture** `crates/ec-av1/fixtures/mix176_offtile_chroma_clip.obu`, **4 581 B**,
FNV-1a64 `0x5e31be5efc9c3c1d`. It is frame 0 of `mix_176x144_6.obu` — the
temporal delimiter, sequence header and frame-0 OBU, nothing else — so the gate
decodes cleanly in isolation instead of depending on a stream that a second
unfixed defect refuses. Full-stream provenance (bytes + sha256) is in the gate's
doc comment.

**Gate** `stream::tests::a_420_lossless_rect_chroma_walk_is_bounded_by_the_chroma_plane`:
pinned fixture, 8-bit header asserted, clean decode, off-tile counter nonzero
(non-vacuity), **unit count == 2373 == the oracle's own `EC_ECDUMP_IN`
census**, then `decode_all_frames_vs_oracle`.

```text
CARGO_TARGET_DIR=$HOME/.cache/cargo-target-av1offtile EC_NOMEMGUARD=1 \
  cargo test -p ec-av1 --lib a_420_lossless_rect_chroma_walk_is_bounded_by_the_chroma_plane

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 803 filtered out; finished in 0.03s
```

**Mutation proof** — the clip surgically reverted in the working tree, gate re-run:

```text
(a) width half disabled  (`if false && cpx + cu_col*4 >= u.true_width || ...`)
    paniced at crates/ec-av1/src/stream.rs:52195:
    frame 0 walked 2381 transform units, the oracle's own EC_ECDUMP_IN census for
    these bytes is 2373
    test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 803 filtered out

(b) clip removed outright
    paniced at crates/ec-av1/src/stream.rs:52186:
    no lossless rect chroma unit fell outside the chroma plane -- the corrected
    walk never ran, so this gate is measuring the unclipped one
    (class gate-blind-to-feature)
    test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 803 filtered out
```

2381 and 2413 are both measured on real builds (§2, and the clean
`658d37fa`-only binary reads 2413), so (a)'s number is not a fitted constant.

**Scoped regression** — exact command and output in §10.

## 6b. JOINT REPRODUCTION: Emre-5's 640x480 `tx_size_cat0` ctx finding

Emre-5 (lane-av1mixllconj) localised the 640x480 residual that remains after
their var-tx conjunct to a third defect and asked me to reproduce it on my own
tree rather than repeat it as an attributed claim. **Reproduced in full**, on a
scratch worktree at their tip `9abe724a` with this commit's `decode.rs` patch
applied on top (`git apply --3way`, clean).

**The method detail that matters, because I got it wrong first time:** their
line `decode.rs:47264` is the **`cat0`** arm of `decode_leaf_rect8`; the
`tx_size_cat1[ctx]` call is a different site (`decode.rs:18835`). I keyed the
override on the `cat1` site first and got **five identical 521 856-read runs for
ctx 0..4** — the override never fired at all. Re-keyed on the `cat0` site:

```text
EC_TXCTX_SITE mi_row=80 mi_col=110 bw=8 bh=4 ctx=2      <- ours, natural
```

| forced ctx | our reads | locked vs oracle | oracle reads |
|---|---:|---|---:|
| natural (=2) | 521 856 | first divergence **179 378** | 457 907 |
| 0 | 286 327 | first divergence 179 378 | 457 907 |
| **1** | **457 907** | **457 907 — bit-identical over ALL of them** | 457 907 |
| 3 | 179 378 | first divergence 179 378 | 457 907 |
| 4 | 179 378 | first divergence 179 378 | 457 907 |

Lockstep compares the **convention-free** fields only — `pre[0]`, `pre[1]`,
`n`, `s`, `post_rng`. `pre[2]` is a bit counter carrying a per-site constant
offset and `cdf0` the 32768-x ICDF mirror; neither is signal. The comparator's
third field is `-?\d+`, not `\d+`: **211 of the oracle's 457 907 lines carry a
NEGATIVE `pre[2]`** and a `\d+` field drops them silently.

**The operands, from this tree's own rungs:**

```text
ORACLE  EC_TXCTXB mi=80,110 bsize=2 maxw=8 maxh=4 hasup=1 hasleft=1 abv=4  lft=8  above=0 left=1 ctx=1
OURS    EC_TXCTX  mi=80,110 own=8x4 ha=true hl=true intra_only=false above_txfm=16 left_txfm=8
                                          above=true left=true          ctx=2 (natural)
```

`left_txfm = 8` matches the oracle's `lft = 8`. The whole disagreement is the
**`above` TERM**, driven by the band (**16 vs 4**) — not by the context formula.

**The band write, replayed.** `EC_TXUPD` (`rect` and `ctxs` rungs together),
restricted to the frame containing the fork and to writes whose column span
covers mi column 110 with `mi_r < 80`, in decode order:

```text
ctxs mi=(20,108) tx=16
ctxs mi=(24,104) tx=32
ctxs mi=(32,108) tx=16
ctxs mi=(36,108) tx=16 skip_inter=true   <- the last write before the read
```

A **skipped 16 px inter block** at `mi=(36,108)` with `w_mi=4` publishing **16**,
and **nothing writes that column in between**. libaom has **4** there — it makes
a band write in rows 37..79 that this tree does not. Class
`context-band-not-published`.

**What reproduces and what does not.** Every number, the operand pair, and the
named band write reproduce exactly. **The forced `ctx` is a symptom mask and
neither of us ships it** — the defect is the missing band write. This lane
changed no production code for it: the override was a temporary env-gated probe
in a throwaway worktree and is not in this commit
(`git show HEAD:crates/ec-av1/src/decode.rs | grep -c EC_TXCTX_SITE` is `0`).
The finding belongs to Emre-5's lane.

**Precondition, and it is load-bearing.** The override only bites **with their
var-tx conjunct landed**. On this lane's tree alone (`main` + this clip, no
conjunct) `mix_640x480_5.obu` reads **860 431** times against the oracle's
457 907, and ctx 0..4 at the keyed site give **860 431 for every value** — the
walk never reaches `mi=(80,110)` in a state where the override matters. The
chain is strictly ordered: off-tile walk (this lane) -> var-tx conjunct
(Emre-5) -> TXFM_CONTEXT band gap (Emre-5).

## 6c. Re-measure PROTOCOL for the 640x480 cell (word-for-word with `lanes/av1mixllconj.report.md` §4.3)

**Two reports describing the same discriminator in different words is how
outcome 2 gets argued away.** This table is the canonical text; the twin in
`lanes/av1mixllconj.report.md` is byte-identical. Copy it, do not paraphrase it.

**The pixel run cannot discriminate the two candidate causes.** A green
byte-exact compare against the oracle is EQUALLY consistent with the TXFM band
write being closed and with the forced-`ctx=1` mask doing the work — because
that mask makes the cell exact on its own (§6b: ctx=1 gives 457 907/457 907
bit-identical without any band change). The pixel run reports the CONSEQUENCE.
The operand pair is the cause, and the pair is readable from rungs that already
exist **before the fix lands**.

**Earliest discriminator, no decode needed — read the diff first.** A
band-write site touched with `decode_leaf_rect8` untouched settles it: the band
was the cause. The reverse settles the other way: the band was a symptom and
§4.3 was directionally right but causally wrong.

**Then the operand pair**, re-read on the same two rungs at `mi=(80,110)`:

```text
oracle  EC_TXCTXB mi=80,110 abv=4 lft=8 above=0 left=1 ctx=1
ours    EC_TXCTX  mi=80,110 above_txfm=? left_txfm=8 above=? left=true
```

| # | `above_txfm` after the fix | `ctx` after the fix | reading |
|---|---|---|---|
| 1 | **4** | **1** (reached on its own) | The band write at `mi=(36,108)` was publishing 16 where libaom has 4. The fix closed the band; the `ctx=1` lock is **retired as a symptom mask** and the chain off-tile walk -> var-tx conjunct -> band gap is confirmed end to end. |
| 2 | **16** | 1 | **The dangerous one.** The fix was in the ctx formula or the arm, the band is still wrong, and the cell's exactness is a MASK, not a repair. A green pixel compare reports this as success. |
| 3 | 4 | not 1 | A further defect remains. The cell should NOT go exact; if it does, another mask is in play. |

**Assert this, do not log it.** The table's outcomes are an assertion, not a
line a reader has to notice — outcome 2 in particular passes a pixel compare
cleanly, so an assert is the only form that fails loudly.

**Comparison conventions for the run**, so the numbers are comparable with the
ones already in these reports:

* Compare on the **convention-free** `EC_SYMR` fields only — `pre[0]`, `pre[1]`,
  `n`, `s`, `post_rng`. `pre[2]` is a bit counter carrying a per-site constant
  offset and `cdf0` the 32768-x ICDF mirror; neither is signal.
* The third field's regex is **`-?\d+`**, not `\d+`. The oracle prints a
  NEGATIVE `pre[2]` on a stream-dependent fraction of lines (211 of this
  stream's 457 907; 52 of 96 997 on the 256x128 pin) because it tracks
  bit-counter wraparound. A `\d+` field drops them and the pair count reads
  short, which looks like a length mismatch rather than a comparator bug.
* **Name which override sweep produced any ctx table.** Per-site (keyed on one
  `lmi`) and global (every site of the category) give different non-answer rows
  without contradicting: a global override perturbs sites that are not the fork.
  §6b's table is PER-SITE.
* Resolve line numbers against the tree you are on and record the **FUNCTION**,
  not the line: the site is the `tx_size_cat0` arm of `fn decode_leaf_rect8`,
  and a merge moves every line below it.

**Ordering, as evidence rather than sequence.** This lane's fix moved 176x144's
first divergence from read 25 792 to 29 411 and changed **nothing** on 640x480;
Emre-5's conjunct is what makes `mi=(80,110)` reachable in a state where its
context matters at all. Those two facts are what make the chain ORDERED rather
than merely sequential — and they are also why the operand pair is readable
pre-fix.

## 7. Class sweep

**Source.** Every chroma unit walk in `decode.rs` that derives a unit count from
the block footprint, each re-derived against `max_block_wide/high`:

| site | walk | verdict |
|---|---|---|
| `decode_rect_split` luma, `for tu_row in 0..bh/tx_h { for tu_col in 0..bw/tx_w }` | reads coefficients | **carries the clip** (`tu_px >= y.true_width`), correct |
| `decode_rect_split` **lossless chroma, `for plane_idx 1..=2 { for cu_row in 0..ch_n { for cu_col in 0..cw_n } }`** | reads coefficients | **this lane's defect — now clipped** |
| `decode_rect_split` `m.skip` arm, `for cu_row in 0..chroma_block_h/uh` | **no reads** (writes a zero grid) | not entropy-relevant |
| `decode_rect_split` `chroma_tiled` arm, `(nw, nh) = (chroma_block_w/uw, chroma_block_h/uh)` | reads coefficients | instrumented, **0 hits** (§7b) |
| `decode_block_rect64`, `(nw, nh) = (chroma_w/uw, chroma_h/uh)` | reads coefficients | instrumented, **0 hits** (§7b) — Selin-7's region, probed read-only, not edited |
| `rect_inter_chroma`, `(nx, ny) = (write_chroma_w/uw, write_chroma_h/uh)` | inter chroma rect walk | not a rect/lossless intra arm; no overhang witness reachable (§7b) |

**Streams.** The two non-fixed coefficient-reading walks above were temporarily
instrumented with the same `>= u.true_width / u.true_height` test and swept
over:

* the **whole committed fixture corpus** (`crates/ec-av1/fixtures/*.obu`, every
  file the crate ships) — **0 hits**;
* **18 generated edge-overhang cells** — `testsrc2` at 178x146, 182x150, 190x158,
  200x180, 176x142, 144x176, 210x134, 128x128, 320x244, each at
  `--cq-level=0 --aq-mode=1` and `--lossless=1` (the two arms that put a
  lossless strip at a frame edge) — **0 hits**, and all 18 decode byte-exact
  against the oracle;
* the 12 `mix_*` cells plus `c_mix5.obu` — **0 hits**.

**This is a measured negative on the corpus and on every edge geometry I could
generate, not a proof of unreachability.** The instrumentation has been removed;
`grep -c EC_OFFTILE crates/ec-av1/src/decode.rs` is `0`.

## 8. Hunk scope

* `crates/ec-av1/src/decode.rs` — **2 logic lines** (the clip + its
  `hit!`), plus the counter, its two accessors, and `census_unit_n` /
  `reset_census_unit_n`.
* `crates/ec-av1/src/stream.rs` — the gate, no production code.
* `crates/ec-av1/fixtures/mix176_offtile_chroma_clip.obu` — the pinned fixture.
* **Not touched:** `decode.rs:48259`, `decode.rs:27140`, `decode_block_rect64`
  (`19634-20315`, Selin-7's), and the var-tx `lossless` derivation. Peer scope
  was announced over IRC before the first edit and held throughout.
* The primary checkout was never edited; `git status` there is clean apart from
  peers' own branches.

## 9. What is NOT measured

* **This commit does not remove the refusal on `mix_176x144_6.obu` /
  `mix_352x288_6.obu` by itself** (§5). It removes the off-tile walk and moves
  the first divergence from 25792 to 29411. Closing the cell needs Emre-5's
  conjunct, and the composition is measured but not landed by me.
* **The 640x480 `658d37fa` regression is untouched by this lane and not
  diagnosed here.** With both fixes applied 640x480 **REFUSES** (Emre-5 measured
  the same); my patch changes nothing on that cell. Emre-5 further localises the
  640x480 residual to a **third** defect — a missing TXFM_CONTEXT band write
  above mi column 110 (`decode.rs:47264`, forcing `ctx=1` there makes 640x480
  bit-identical over all 457 907 reads), class
  `context-band-not-published`, which is theirs to carry and is not mine.
  **§6b is my own full reproduction of that claim** — the read counts, the
  457 907/457 907 lockstep, all three operand prints, and the `mi=(36,108)`
  band write — so it is now a joint fact on two trees rather than an attributed
  one.
* **The class sweep is a corpus + generated-cells negative**, §7. A wider sweep
  (10/12-bit, 4:4:4 edge cells, screen content) was not run; §7's instrumented
  walks are the ones a future sweep should re-arm.
* **Frame 0's pixels are byte-exact at base too**, so nothing here is a pixel
  claim: the 40 phantom reads all carry `all_zero` and consume no data. The
  gated number is the unit census, not the picture.
* The `mix_352x288_6.obu` ladder was not paired read-by-read (its first
  divergence is the same var-tx site); the cell's byte-exactness with both
  fixes is measured, its per-read attribution is not.

## 10. Regression result

Exact command, run in the lane worktree at the fix commit:

```text
CARGO_TARGET_DIR=$HOME/.cache/cargo-target-av1offtile EC_NOMEMGUARD=1 \
  cargo test -p ec-av1 --lib -- 420 444 422 lossless stream \
  --skip bitrate_target_lands_within_5_percent_over_48_frames
```

```text
test result: ok. 375 passed; 0 failed; 7 ignored; 0 measured; 422 filtered out;
             finished in 1970.18s
```

`375 / 0 / 7 / 422`. The two prior reports that ran this exact scope on
`32d944bf` record `373 / 0 / 7 / 422`, so this is `+2`: one of them is this
lane's new gate and the other is a test that landed on `6bb66a4a` after those
runs. The load-bearing part is **0 failed**. The gate itself is in this run
(`a_420_lossless_rect_chroma_walk_is_bounded_by_the_chroma_plane ... ok`).

## 11. Fix-now | deferred | accepted

* **fix-now:** nothing outstanding in the tree. The clip, the counter, the gate
  and the fixture are all in this commit.
* **deferred(unblock: Emre-5 landing the var-tx `lossless` conjunct):** the
  `mix_176x144_6` / `mix_352x288_6` refusal. §5 measures that the two fixes
  compose and make both cells byte-exact against the oracle; neither alone does.
* **deferred(unblock: a >= 640x480 mixed-lossless + altref committed fixture):**
  §5's composition claim is a hand-measured A/B, and the `658d37fa` regression
  it would have caught has no gate. That is a fixture-coverage gap, not this
  lane's fix.
* **accepted:** the two non-fixed chroma walks (§7) stay as they are on this
  corpus. They are the same class and one env-gated probe line away from a
  witness; the probe has been removed and the negative is recorded rather than
  papered over with an assertion.

## 12. Reproduction

```bash
W=~/.cache/wt/av1offtile
P=$HOME/.cache/cargo-target-av1offtile/debug/examples/decode_probe
D=~/.cache/aom-oracle/build
F=$W/crates/ec-av1/fixtures/mix176_offtile_chroma_clip.obu

# the gate
(cd $W && CARGO_TARGET_DIR=$HOME/.cache/cargo-target-av1offtile EC_NOMEMGUARD=1 \
  cargo test -p ec-av1 --lib a_420_lossless_rect_chroma_walk_is_bounded_by_the_chroma_plane)

# the red-before: 2413 units against the oracle's 2373
EC_NOMEMGUARD=1 EC_ECDUMP_IN=1 $HOME/.cache/cargo-target-av1offtile-xb/debug/examples/decode_probe \
  $F 2>&1 >/dev/null | grep -c ECDUMP_IN          # 2413
EC_ECDUMP_IN=1 $D/aomdec --codec=av1 -o /dev/null $F 2>&1 >/dev/null | grep -c ECDUMP_IN  # 2373
EC_NOMEMGUARD=1 EC_ECDUMP_IN=1 $P $F 2>&1 >/dev/null | grep -c ECDUMP_IN   # 2373

# the 40 extra reads, before and after, on the full 6-frame stream
EC_NOMEMGUARD=1 EC_SYMR=1 $P /tmp/g320/sweep/mix_176x144_6.obu 2>our.symr >/dev/null
EC_SYMR=1 $D/aomdec --codec=av1 -o /dev/null /tmp/g320/sweep/mix_176x144_6.obu 2>orc.symr >/dev/null
# base: first mismatch at 25792, ours 25792..25831 extra;  after: first mismatch at 29411
```
