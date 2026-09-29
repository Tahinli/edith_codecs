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
