# lane-av1lm444loss-corr — the ss-aware replay step in `decode_intrabc_rect`

**Verdict.** One line, one real bug, already on main in everything but that
line. `lane-whtshape` (`f541d062`, merged `afa13bcf`) gave `decode_intrabc_rect`
the lossless per-TX_4X4 chroma walk and its tail replay. The replay stepped the
mi origin by `4 >> ss_y(fctx)` — 4:2:0-only arithmetic — so on a 4:4:4 lossless
frame it stamped every replayed unit at **4x its own mi row/column**, writing
over cells that belong to other blocks. Corrected to `(4 << ss) / MI`, the form
the codebase's own reference construction already uses.

**Tree.** `lane-av1lm444loss-corr` off current main `4532caa6`, worktree
`~/.cache/wt/av1lm444corr`. One commit, `c9b48f5d`. Not pushed (Main merges).

## 0. What happened to the previous branch, stated plainly

`lane-av1lm444loss`'s r2/r3 (`5519aaf8`, `6490933c`, `927dbf09`) is
**DO-NOT-MERGE**, and the review is right:

* `decode_intrabc_rect` on main already carries the lossless arm from
  `lane-whtshape` — ss-aware geometry at `decode.rs:13343`/`:13352`, the
  `read_inter_chroma_lossless` walk with `hit!(INTRABC_RECT_LOSSLESS_CHROMA4_HITS)`
  at `:13687`, and the tail replay. My `5519aaf8` re-implemented all of that on
  a stale base (`a21f3680`), unaware of it.
* Resolved "theirs", it would have **deleted** main's
  `INTRABC_RECT_LOSSLESS_CHROMA4_HITS` and replaced main's
  `ll_chroma: Option<(Vec, Vec)>` tail with my `mu_chroma: bool` +
  `Grid::Own(mu_units(...))`.
* My headline — "two defects, both only at 4:4:4 lossless" — was ~80% already
  merged under another lane's name, and my report never mentioned `whtshape`.

So: I worked blind to main. The old branch's own report also carried stale
provenance (it said "off `a21f3680` with one commit `7b9f46fa`" when by r3 it
was off `9249442f` with five) — a report that misstates its own base is a
defect in itself, and that is recorded here rather than patched there.

What survives from the old branch and is carried here: the **fixture** and the
**finding** that the step is wrong. What is dropped: the re-implementation.

This is a new report. The old one's history is left exactly as it was.

## 1. The line

`crates/ec-av1/src/decode.rs`, in `decode_intrabc_rect`'s tail replay, on main
at line 13895:

```rust
// main (WRONG at ss (0,0)):
(mi_r + ur * (4 >> ss_y(fctx)), mi_c + uc * (4 >> ss_x(fctx))),

// this branch:
(mi_r + ur * ((4 << ss_y(fctx)) / MI), mi_c + uc * ((4 << ss_x(fctx)) / MI)),
```

## 2. Mechanism, and the libaom reference for the step

The replay re-stamps each TX_4X4 chroma unit's own coefficient context, because
the block's whole-block `record_split_luma_rect_mi` / `record_rect_mi` above it
re-stamps the plane from ONE composed grid — level = the sum over units, dc =
the top-left unit's sign. libaom does not compose: `decode_token_recon_block`
(`av1/decoder/decodeframe.c:972`) walks 64x64 "mu" chunks and, inside each,
`for (plane..) for (blk_row..) for (blk_col..)` calls `decode_reconstruct_tx`
per unit, and `av1_set_entropy_context` stamps **that unit's** span. So the
replay's job is to place unit `(ur, uc)` at the mi cell that unit really
occupies.

A TX_4X4 chroma unit covers `4 << ss` **luma** pixels. The chroma entropy bands
are luma-mi indexed (`Neighbours::record_mi_chroma`'s convention), so the origin
advances by that footprint expressed in mi:

```
step = (4 << ss_y) / MI          MI = 4
ss (1,1) 4:2:0:  (4 << 1) / 4 = 8/4 = 2 mi
ss (0,0) 4:4:4:  (4 << 0) / 4 = 4/4 = 1 mi
```

which is exactly what `decode_inter_block`'s reference `mu_chroma_units`
construction writes at `decode.rs:43688`:

```rust
let cu_mi = (
    at.0 + cr * ((cu << ss_y(fctx)) / MI),
    at.1 + cc * ((cu << ss_x(fctx)) / MI),
);
```

with `cu = 4` on a lossless frame (the same function's `let cu = if lossless(fctx)
{ 4usize } else { 32usize };`). Its own comment states the rule: "a TX_4X4
chroma unit covers `4 << ss` LUMA pixels ... each unit's own origin sits
`(cu << ss) / MI` mi cells out". The intrabc-rect replay was the outlier that
had `4 >> ss` instead.

The SPAN argument on the same call, `4 << ss_x(fctx)`, was already the
ss-aware form on main. Only the STEP was not — which is why the two sit
adjacently and disagree, and why the bug is invisible at 4:2:0.

## 3. Why 4:2:0 is unchanged — by construction, then measured

At ss (1,1) the old expression is `4 >> 1` = **2** and the new one is
`(4 << 1) / 4` = **2**. The same integer. So the corrected line cannot alter a
4:2:0 decode at all; this is arithmetic, not an empirical hope, and the 4:2:0
battery below is confirmation rather than the argument.

At ss (0,0) the old is `4 >> 0` = **4 mi** = 16 luma px per 4x4 chroma unit,
where the unit covers 4 luma px = 1 mi — a 4x overshoot on both axes. The
replayed states land four mi rows and four mi columns beyond where they belong,
which is sixteen luma pixels out, over cells that belong to other blocks (or to
nothing, past the block's own span).

4:2:2 is refused upstream by name, so there is no third case to reason about.

## 4. Reachability and measurement

Fixture `crates/ec-av1/fixtures/ll444_intrabc_rect_l2.obu` — 121844 bytes,
sha256 `f0de080edce35d34cdd628b71f63dc452f946e8328352b69d3c1da090a54ec67`,
recipe `testsrc2 512x128 yuv444p`, aomenc `--lossless=1 --cpu-used=2
--lag-in-frames=0 --kf-max-dist=100 --limit=6`, 6 frames.

**The fixture does reach the corrected step** — no unit-level stand-in needed.
Measured on main's own build, decode order against the oracle aomdec:

| frame | wrong samples (main) | wrong samples (corrected) | first byte (main) |
|---|---|---|---|
| 0 (key) | **10604** | **0** | **33216** = Y(64,448) |
| 1 | 123987 | 0 | 0 |
| 2 | 171773 | 0 | 0 |
| 3 | 171988 | 0 | 0 |
| 4 | 171783 | 0 | 0 |
| 5 | 151736 | 0 | 0 |

and the entropy ladder goes from forked to **540833 reads on both sides, zero
fork** (ours 540833 / oracle 540833, every read paired on
`(value, range, symbol, post_rng)`).

Note the first divergence is **luma**, on a chroma-context defect. That is the
signature of this bug and worth reading as such: the wrongly-placed chroma
stamps corrupt the `txb_skip_ctx` the NEXT unit reads, the tile desyncs, and
the pixels that move first are whichever block comes next in luma. Anyone
chasing a "luma divergence on a chroma fix" should check the step before
suspecting the reconstruction.

## 5. Gate

`stream::tests::a_lossless_444_intrabc_rect_replay_steps_by_the_units_own_mi_footprint`

| # | assertion | what it rules out |
|---|---|---|
| 1 | `(subsampling_x, subsampling_y, bit_depth) == (0, 0, 8)` from the **parsed sequence header** | at (1,1) the two steps are the same integer, so a 4:2:0 stream cannot fail this gate and every other row would pass vacuously |
| 2 | lane-whtshape's own `intrabc_rect_lossless_chroma4_hits()` is nonzero | the walk really ran, so the corrected expression was evaluated — a gate that never reached its own subject |
| 3 | U and V plane extents are luma-sized | the grid the replay steps mi cells through |
| 4 | `decode_all_frames_vs_oracle`: decode-order frame-COUNT parity, then byte-exactness on every frame | and it covers hidden alt-refs |
| 5 | control: the 128x96 4:4:4 lossless pin must read that counter **0** | whole square blocks have no multi-unit chroma plane block, so the step is not exercised there at all |

The gate uses **main's** counters (`INTRABC_RECT_LOSSLESS_CHROMA4_HITS` and its
`reset_…` helper). Nothing of whtshape's was renamed, removed, or shadowed.

### Mutation proof (run)

Reverting only the step expression, counters and gate untouched:

```
=== MUT corr ===
panicked at crates/ec-av1/src/stream.rs:9396:
  decode-order frame 0 of 6 (6 shown, 0 hidden) differs from the oracle at
  byte 33216 (ours 101 vs 100), 10604 bytes differ
test result: FAILED. 0 passed; 1 failed
MUT EXIT: 101

=== LIVE ===
test ... a_lossless_444_intrabc_rect_replay_steps_by_the_units_own_mi_footprint ... ok
LIVE EXIT: 0
```

File restored from a saved copy with `cp`, verified with `diff -q`, `grep -c "MUT
corr"` = 0.

## 6. Invariants

| group | result |
|---|---|
| new gate | 1 passed (mutation red, §5) |
| `cargo test -- lossless 444` | **47 passed / 0 failed** |
| `cargo test -- intrabc` | **22 passed / 0 failed / 1 ignored** |
| `cargo test -- <17 scoped 4:2:0 lossless + frame-edge>` | **17 passed / 0 failed** |
| `cargo check -p ec-av1` | 0 warnings, 0 errors |
| source-scan guards | untouched — no `include_str!` anchor edited |
| blast radius | one expression in `decode.rs`; the rest of the diff is the gate plus one test-only helper. `decode_intrabc_rect`'s chroma walk, its `ll_chroma` tail and whtshape's counter are **byte-identical to main** |

## 7. For the reviewer

- **Provenance.** This branch is off main `4532caa6` and contains exactly one
  commit. The old `lane-av1lm444loss` branch is not to be merged; this one
  supersedes it and duplicates none of it.
- **4:2:0.** Stated as arithmetic first (`4 >> 1` == `(4 << 1)/4` == 2) and then
  measured (§6). The gate deliberately does **not** claim a 4:2:0 arm: a 4:2:0
  stream cannot fail it, and a floor-on-wrong-output assertion there would be
  theatre.
- **Not speculative.** The step is reachable and was measured wrong on main
  before the edit (§4): 10604 samples in the key frame, 123987..171988 in the
  inter frames.
- **Citation.** The step rule is taken from this codebase's own reference
  construction (`decode.rs:43688`) rather than re-derived only from libaom, so
  the change matches the shape every other replay in the tree already uses.

## 8. Handover of the 256x256 cell, and a retraction

The 4:4:4 lossless 256x256 cell is a **second defect**, untouched by this
branch and by the superseded `lane-av1lm444loss` branch (identical wrong-sample
count, first sample and magnitude set on both). Handed to **`Aras-2`**, who is
already working that cell and whose pinned tripwire `(1, 201, 200, 160, 159)` is
the same first sample this lane measured. Sent 2026-09-29, in the message
thread on `agent://Aras-2`.

Fixture for reproduction: `testsrc2 256x256 rate=25 yuv444p` (rate matters — my
rate=1 encodes of the same geometry were byte-exact **at base**), aomenc
`--lossless=1 --sb-size=128 --passes=1 --threads=1 --row-mt=0
--enable-palette=0 --lag-in-frames=0 --kf-max-dist=100 --limit=6`, 109909 bytes.
Signature: key frame only, chroma only, 49 samples (Y=0, U=21, V=28), first
U(200,201) ours 160 vs ref 159.

### 8.1 Retraction: "the mi labels are transposed" was wrong

This lane's earlier report flagged the ladder's first fork at read 125511 with
a caveat that the differing `mi` labels looked like a coordinate-grid artefact
rather than a decode divergence. **That caveat is withdrawn.** Three
measurements, taken before the handover:

1. **Both traces print mi in the same field order.** The oracle prints
   `ec_symr_mi_row, ec_symr_mi_col` (`aom_dsp/bitreader.h:265-266`); this
   decoder prints `SYMR_MI.get().0, .1` (`msac.rs:476-478`). There is no
   field-order swap to explain anything.
2. **It is not a transposition of the same position either.** Ours reads
   `mi=(32,0)`, the oracle `mi=(0,48)`; swapping ours gives `(0,32)`.
3. **The label tracks the walk closely**, so its first disagreement is signal.
   The two labels agree on **90112 of the first 125511 reads (71.8%)**.

At the fork both sides are at `ph=inter`, where both publish **block-level** mi
(ours from `decode_inter_block`'s `at`, `decode.rs:38706`; the oracle from the
mode-info reader's entry, `decodemv.c:941`) — the same granularity — and they
still disagree, off an identical pre-state `(24645, 40117)` with different
post-ranges (39316 vs 39862). So read 125511 is a **genuine block-level walk
divergence**.

The generalisable lesson is the mirror of the one in
`skill://ec-av1-divergence-debug`: a "the tag disagrees, so it is a labelling
artefact" reading is a hypothesis to be TESTED, not a conclusion to be
forwarded. This lane forwarded one. Testing it cost three cheap measurements
and reversed the conclusion.

### 8.2 One more cross-side trap found in the same ladder

The **phase vocabulary differs between the two decoders**. This decoder emits
`ph=inter8` for the sub-8x8 leaf arm (`decode.rs:47162`) — 13894 reads on this
stream — and the oracle has **no `inter8` phase at all**: its `ec_symr_phase`
is set at the mode-info reader's entry and lumps those reads under the block's
own phase. So `ph` is not a pairable field across the two sides either; only
`ph=inter` against `ph=inter` compares.

## 9. Two findings recorded for the batch's shared lore

**9.1 A luma first-fork is the signature of a chroma arithmetic error.** The
bug this branch fixes is a purely CHROMA error — an mi step on the chroma
context replay — yet the first wrong sample on the witness stream is **luma**,
`Y(64,448)`, in the key frame. Mechanism: the wrongly-placed chroma stamps
corrupt the `txb_skip_ctx` the NEXT unit reads, the tile desyncs from there, and
whichever block comes next in luma is what moves first. This is the shape that
makes a correctly-applied fix look ineffective: the gate goes green and the
diff you were watching was never where the bug was. Whenever a chroma-side
arithmetic fix appears not to have moved a LUMA first-fork, check the chroma
step before suspecting the reconstruction or the filter chain.

**9.2 The non-vacuity discipline this gate uses, restated.** Two rows do the
real work and both are about *reaching* the corrected expression, not about
its value:

* the **parsed-header** assert pins `(0,0,8)` from the stream's own sequence
  header precisely *because* at `(1,1)` the old and new steps are the same
  integer — a 4:2:0 stream cannot fail this gate at all, so without the header
  row every other row would be asserting nothing;
* the **control pin** reads the counter 0 on a stream of whole square blocks,
  because those have no multi-unit chroma plane block to step — which is what
  makes "the counter fired" mean "a multi-unit step was actually taken".

Together those two mean the gate fails if the step is wrong AND fails if the
step is never reached. Neither alone would.

## 10. The 256x256 key frame is RECONSTRUCTION-ONLY (measured, after handover)

`Aras-2` asked the decisive question of this cell and it is answerable, so it
was answered rather than left open. **My handoff was incomplete in a way that
mattered, and the correction is recorded here.**

### 10.1 Per-frame read counts, and where the fork actually is

On this lane's 109909 B stream (`testsrc2 256x256 rate=25`, `--sb-size=128
--passes=1 --threads=1 --row-mt=0 --enable-palette=0 --lag-in-frames=0
--kf-max-dist=100 --limit=6`, intrabc ENABLED — `Aras-2`'s 109215 B stream has
`--enable-intrabc=0` and default sb-size, so the two really are different
streams, as they noted):

| decode_idx | EC_SYMR reads | cumulative |
|---|---|---|
| 0 (**key**) | **82697** | 82697 |
| 1 | 50717 | 133414 |
| 2 | 120835 | 254249 |
| 3 | 62579 | 316828 |
| 4 | 102367 | 419195 |
| 5 | 121957 | 541152 |

**First fork: read 125511 — inside `decode_idx=1` (82698..133414), not the key
frame. First fork within the key frame's 82697 reads: NONE.**

### 10.2 What that settles

The key frame's 49 chroma samples (first `U(200,201)` ours 160 vs ref 159) are
**reconstruction-only**. That frame's entropy is bit-identical end to end, so
nothing on the entropy side can be their cause — not the mu-chunk walk, not the
plane threading, not the coefficient sets, not the transform-size derivation.
`Aras-2`'s r1 walk fix and r6 plane threading are therefore not the cause of
the 49, and their NOT-SETTLED "per-unit coefficient SETS agree or not" question
— which is an entropy question — cannot answer it either.

Two independent encodes (109909 B and 109215 B, different `--sb-size` and
intrabc settings) both show a bit-locked key frame and a ~49-sample
chroma-only key-frame divergence. That is one confirmed defect, and it is on
the **reconstruction** side: after `decode_token_recon_block`, so at 4:4:4
lossless the WHT path (`dequant_and_inverse_wht4x4` / `TxParams::run`), the
prediction fetch, or the post-recon filter chain.

### 10.3 The correction to my own handoff

I told `Aras-2` that read 125511 was "a genuine block-level walk divergence".
The mi-label retraction (§8.1) stands, but that framing was still wrong in
substance, because I had not measured the frame boundary when I sent it. The
cell has **two symptoms, not one**:

* **(a)** key frame, reconstruction-only, 49 chroma samples;
* **(b)** frame 1+, an entropy fork at read 125511.

They are not linked by reconstruction: frame 1's entropy does not depend on
frame 0's pixels, and the CDF state carries across frames untouched by them.
So **(b) is its own entropy defect** — `Aras-2`'s r1 walk fix closed their (b)
at read 123121 without closing (a). `Aras-2`'s ordered-(plane, position,
nz-count) sequence diff remains the right decisive test for (b); for (a) the
search space is now confined to the reconstruction stage.

### 10.4 A pairing-script bug worth recording

My first ladder script compared the `cdf0` field directly across the two sides.
That is the one field whose convention differs — ours is PRE-adapt, the
oracle's is POST-adapt, and the reconciliation is `32768 - ours_cdf0 ==
theirs` (`msac.rs`'s own `EC_SYMR` doc). The corrected predicate is
`(value, range, n, symbol, post_rng)`, with the `bit` field carrying a constant
-15 offset on this stream. Re-running with the right field set **confirmed
125511** — so the number I reported stands, but it survived a script that could
not have been trusted to find it. A fork index from a script with a known-bad
comparison field is a coincidence until re-derived.
