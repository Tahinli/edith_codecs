# lane-av1leftwit — is the left chroma reference's `+ss_y` term witnessed?

Base `a21f3680`, worktree `~/.cache/wt/av1leftwit`, branch `lane-av1leftwit`.
One commit, nothing pushed. Assignment: the shipped `+ss_y` left-reference
change has no witness, which is a correctness hole, not a coverage gap.

## 0. Verdict

**The premise behind the shipped comments is false, and the term itself is
correct.** Two separate claims were standing in for evidence; both were
checked here and neither survives:

1. "The shape the term needs is structurally absent, because aomenc never
   starts a block on an odd mi row" — **false, twice over.** 1 mi is 4 luma
   pixels, not 8; libaom's `PARTITION_HORZ_4` places its `i == 0` child on the
   PARENT's own mi row, which for a 16x16 is a multiple of 4 and therefore
   even; and the shape is not hypothetical — it reaches a real decode-path
   read in **5 of the 48 committed fixtures** (15 reads in total), which is
   what the new `uv_left_1mi_split_hits()` counter measures.
2. "Six committed 4:2:0 fixtures reach it, and on `hg_ss600_key_frame.obu` the
   boolean flips" (a claim in `decode.rs`) — **never measured, and false.**
   `hg_ss600_key_frame.obu` reads `mode_diff=0 smooth_diff=0 split1mi=0`.

What is still unwitnessed is narrower and is now stated correctly: in every
one of the 15 decode-path reads that see the shape, the two blocks **agree on
`uv_mode`**, so the term still changes no pixel. A 115-encode targeted hunt
(§3) plus the whole committed corpus did not move that. The `+ss_y` term is
kept — it is verified against the oracle source, not against a corpus
(§1) — and this lane's job is to make the hole smaller, named and gated
rather than propped up by a counter whose call site no decode path uses (§7).

## 1. The term, against the oracle source (not a corpus)

`~/.cache/aom-oracle/src` @ `v3.13.3-7-g9bb526a`,
`av1/common/av1_common_int.h:1393-1412` (`set_mi_row_col`):

```c
  const int chroma_ref = ((mi_row & 0x01) || !(bh & 0x01) || !ss_y) &&
                         ((mi_col & 0x01) || !(bw & 0x01) || !ss_x);
  xd->is_chroma_ref = chroma_ref;
  if (chroma_ref) {
    MB_MODE_INFO **base_mi =
        &xd->mi[-(mi_row & ss_y) * xd->mi_stride - (mi_col & ss_x)];
    MB_MODE_INFO *chroma_above_mi =
        xd->chroma_up_available ? base_mi[-xd->mi_stride + ss_x] : NULL;
    MB_MODE_INFO *chroma_left_mi =
        xd->chroma_left_available ? base_mi[ss_y * xd->mi_stride - 1] : NULL;
```

`chroma_left_mi = base_mi[ss_y * stride - 1]` — the shipped read
`left_mi = (mi_r + ss_y, mi_c - 1)` is this line, with the caller's
`(mi_row & ss_y)` snap already folded into `mi_r`. **The term is right**, and
so is the shape claim about the syntax:

* `av1/decoder/decodeframe.c:1334` — `partition = (bsize < BLOCK_8X8) ?
  PARTITION_NONE : read_partition(...)`: a sub-8x8 bsize is a leaf or nothing.
* same file, `PARTITION_HORZ_4` — `this_mi_row = mi_row + i * (bw / 4)`. A
  16x16 (4x4 mi) sits on a 4-mi grid, so its `i == 0` child is **1 mi tall at
  an even mi row**. The geometry the hunt needed is what the syntax produces
  first, not something it forbids.
* `av1/common/common_data.c:17` — `av1_ss_size_lookup[.][1][1]` is non-INVALID
  for `BLOCK_4X4/4X8/8X4/8X8/16X4/32X4/64X4`, so 4-px-tall leaves are legal
  at 4:2:0 (and 4:4:0, `ss_y == 1`).

One nuance worth keeping: `chroma_ref` is **false** for a block that is itself
1 mi tall (`bh` odd) on an even row at `ss_y == 1`, i.e. libaom does not
*publish* such a block as a chroma reference. It does not block the shape
this lane is about: the block that READS the left neighbour is >= 2 mi tall
(`bh` even) whenever it spans both cells, so its own `chroma_ref` is true and
it does apply the offset.

## 2. The instrument (what makes the shape measurable at all)

`uv_left_ss_y_reads` counts reads with a left neighbour — it cannot tell
"reached the shape" from "read two cells of the same block". Three additions,
all in `decode.rs`, none of them on a decision path:

* `Neighbours::uv_owner: Vec<u32>` — per mi cell, `(epoch << 24) | (owner
  mi_r << 12) | owner mi_c` of the block that wrote it, stamped in
  `record_uv_mode_mi` over the same span and with the same clipping as
  `uv_mode_grid`. The epoch (`UV_OWNER_EPOCH`) is bumped in `start_tile`, so
  a cell no block wrote in this tile reads 0 and can never be compared.
* `UV_LEFT_1MI_SPLIT_HITS` / `uv_left_1mi_split_hits()` — incremented only
  under the existing `decode_path` guard, when both cells were written in this
  tile **and by different blocks**. That is the run-of-length-1 geometry, and
  it is the precondition for the term being observable at all.
* `EC_LEFTSSY=1` traces: `EC_LEAF mi=(r,c) wh=(w,h) uv_mode=N` per recorded
  block (the parse-side leaf list) and `EC_LEFTSSY ... owners=(..)->(..)` per
  split read. These are what the hunt below was steered by — the leaf list
  says what the partition tree actually contains, before any pixel argument.

Cost: one extra `u32` per mi cell and one `fill_span` per recorded block
(~0.5 MB for 4K). A `u8` extent grid was tried as well and **removed** — it
existed only for the trace and was avoidable work in the hot path.

## 3. Hunt

115 encodes on the durable oracle, 4:2:0 8-bit, all with
`--enable-1to4-partitions=1`, `--cpu-used=0`, `--limit=1`, `--threads=1
--row-mt=0 --lag-in-frames=0` unless the row says otherwise. "parsed tree" is
the `EC_LEAF` census of that stream (leaf extents in mi); `h=1` counts
1-mi-tall leaves, the only shape that can make the two cells differ.

| recipe | parsed tree | decode-path counter | pixel verdict |
|---|---|---|---|
| synthetic 8-px chroma bands (`mix4/flat/vgrad/hgrad`), 64x64..256x128, cq 0(lossless)/8/12/20/24/32/36/45, max-part 32 | 2/4/8 mi tall, `h=1` = 0 | reads 4..372, `split1mi=0` | n/a — shape never reached |
| `brick`: 4-px (1 mi) bands, vertical 8-px stripe pattern with the phase shifted per band, chroma flat/diagonal alternating per 2x2 chroma cell; 5 geometries x cq 0/20/32, max-part 32 | 2x2/4x2/2x4/8x4 leaves, `h=1` = 0 | `split1mi=0` | n/a |
| `noise2` (2-px cells — sub-transform, so no block size codes it cheaply) and `noise1` (4-px cells), min-part 4, max-part 8/16/32/128, lossless + cq 30/45 | 2x2/4x2/2x4/4x4, `h=1` = 0 | `split1mi=0` | n/a |
| `mosaic` (pseudo-random 8x8 cells, chroma flat vs ramp alternating per 4x4 cell), cq 20/32 | 2x2/4x2/2x4, `h=1` = 0 | `split1mi=0` | n/a |
| real lavfi: testsrc2 / mandelbrot / cellauto / life / rgbtestsrc / smptebars, 320x240 + 640x480, cpu-used 0 and 4, max-part 32 and 64, cq 24, 3 frames (36 arms) | 2x2 upward, `h=1` = 0 | `split1mi=0` in all 36 | n/a |
| **committed corpus, 48 fixtures that decode** | see §4 | **`split1mi > 0` in 5** | both pinned arms exact (kf900: 22292 reads, all three planes) |

The lesson the leaf census teaches, and the reason the sweep stops where it
does: a wide block codes 1-D structure almost for free. A 32x16 leaf spans
four 1-mi bands and still wins, because a vertical step is one coefficient per
4x4 transform block however tall the block is (`brick`, `mix4`); and content
fine enough to force small blocks stops at 8x8 (2x2 mi), which is 2 mi tall
and therefore cannot split a cell pair either (`noise2`, `mosaic`). The
encoder reaches 1-mi-tall leaves only on real material — which is exactly what
the committed film cut shows and what no lavfi source reproduced.

## 4. Corpus census — the shape is on the decode path

Every `.obu` in `crates/ec-av1/fixtures/`, measured with the pinned probe
(`decode_probe` prints `uv_left_ss_y: reads=.. mode_diff=.. smooth_diff=..
split1mi=..`). 48 produced numbers: **30 read the term** (4:2:0, `ss_y == 1`),
6 refused (4:2:2, the unconditional refusal — the term is a no-op there by
construction), and 12 decoded with **0 reads** because they are 4:4:4
(`ss_y == 0`, so the term is literally the same cell as before). Every fixture
that can reach the shape is in the 30.

| fixture | format | reads | mode_diff | smooth_diff | **split1mi** |
|---|---|---|---|---|---|
| `gm_small_side_witness.obu` | yuv420p10le | 13311 | 0 | 0 | **5** |
| `troy_sb128_inter_witness.obu` | yuv420p10le | 5472 | 0 | 0 | **7** |
| `av112bit-inter.obu` | yuv420p12le | 40 | 0 | 0 | **1** |
| `av112bit-compound.obu` | yuv420p12le | 62 | 0 | 0 | **1** |
| `av112bit-compound-masked.obu` | yuv420p12le | 65 | 0 | 0 | **1** |
| `hg_kf900.obu` | yuv420p10le | 22292 | 0 | 0 | 0 |
| `hg_ss600_key_frame.obu` | yuv420p10le | 6297 | 0 | 0 | 0 |
| (25 other `ss_y == 1` fixtures) | 4:2:0 | 4..14537 | 0 | 0 | 0 |

The 15 split reads, verbatim from the trace (`gm_small_side_witness`):

```
EC_LEFTSSY epoch=1 mi=(84,356)  left_col=355 owners=(84,354)->(85,354) modes=0/0
EC_LEFTSSY epoch=1 mi=(96,216)  left_col=215 owners=(96,215)->(97,215) modes=0/0
EC_LEFTSSY epoch=1 mi=(18,176)  left_col=175 owners=(18,189)->(19,189) modes=0/0
EC_LEFTSSY epoch=1 mi=(74,296)  left_col=295 owners=(74,294)->(75,294) modes=0/0
EC_LEFTSSY epoch=1 mi=(184,376) left_col=375 owners=(184,374)->(185,374) modes=0/0
```

Every one of them: the upper cell's block ends at that row, a second block
starts immediately below it in the same column, and the reading block's
`(row, col-1)` / `(row+1, col-1)` cells are those two blocks. Measured extents
at those sites were `(4x2)` over `(4x1)` mi — a 32x16 block with a 16x4 block
underneath it, which is the `PARTITION_HORZ_4` shape of §1, at even mi rows.
`epoch=1` for all five: first tile, first frame.

So: **the geometry is real, common enough to pin, and libaom-correct.** What
is missing is a case where the two blocks disagree on `uv_mode`; in these
streams 97.6% of recorded blocks carry `DC_PRED`, and 1-mi-tall leaves sit on
high-detail edges where chroma is flat, so the two conditions are close to
anti-correlated in practice. That is the honest residual, and it is a
statement about the corpus, not about the syntax.

## 5. Red / green

* **Gate non-vacuity (the new gate).** `stream.rs::
  a_committed_4to20_stream_presents_a_one_mi_left_neighbour_to_a_decode_path_read`
  asserts `uv_left_1mi_split_hits() == 5` on `gm_small_side_witness` and
  `== 7` on `troy_sb128_inter_witness` (deltas around the decode), plus frame
  count and dimensions. **Green:**
  `5 1-mi-tall left neighbours in 13311 decode-path reads, +ss_y changed the
  mode 0 and the smooth boolean 0 times` / `7 ... in 5472 ...`.
  **Red:** the `uv_owner` stamp was removed for one build (`if false {
  fill_span(...) }`), both arms fell to 0 and the gate FAILED on the census.
  Reverted, green again. The mutation is deliberately *not* the `+ss_y` term:
  the term's own mutation leaves these reads untouched, which is the limit of
  what this fixture set can decide.
* **The term's mutation (pixels).** §6.
* **The existing kf900 gate still passes** with the new counters in the tree:
  `22292 left-reference reads, +ss_y changed the mode 0 and the smooth boolean
  0 times, all three planes pixel-exact`.

## 6. The term's own mutation, on the five streams that carry the shape

`left_mi = (mi_r + ss_y, ..)` -> `(mi_r + 0 * ss_y, ..)` (the pre-lane
own-row read), `EC_PROBE_OUT16` dumps (yuv420p10le) from both builds, byte
compare:

| fixture | shipped vs mutated |
|---|---|
| `gm_small_side_witness.obu` | IDENTICAL (150543360 B) |
| `troy_sb128_inter_witness.obu` | IDENTICAL (68428800 B) |
| `av112bit-inter.obu` | IDENTICAL (122880 B) |
| `av112bit-compound.obu` | IDENTICAL (368640 B) |
| `av112bit-compound-masked.obu` | IDENTICAL (368640 B) |

All five are byte-identical with the term removed. That is expected, and it
is the point: the shape is reached 15 times and the term still moves nothing,
because the two blocks agree on `uv_mode` in every one of those reads.
Reproduce with:

```sh
cargo build -p ec-av1 --example decode_probe
EC_PROBE_OUT16=/tmp/s.yuv target/debug/examples/decode_probe crates/ec-av1/fixtures/gm_small_side_witness.obu
# then edit decode.rs:9806 to `mi_r + 0 * ss_y`, rebuild, dump /tmp/m.yuv, cmp
```

## 7. Disposition of the phantom mirror

`decode.rs:17642` (the 16x4/4x16 pair witness) calls the unsnapped form with
the leaf's own — **odd** — mi row and feeds `RECT4_16_UV_PAIR_FILT_HITS`. It
was cited as the `+ss_y` term's reachability evidence, and the `decode_path`
flag exists to keep it out of the term's counters. Measured, that flag's
stated reason ("no committed fixture reaches it on the decode path") is
false, and the mirror is **not** a phantom path either: it measures a real
comparison the decode makes (the pair's chroma-reference read against the
leaf's own row) and it fires — 51 / 24 / 121 / 0 / 1 hits on
`gm_small_side` / `troy_sb128_inter` / `hg_kf900` /
`hg_rect64_intra16x4_witness` / `av112bit-inter`.

**Disposition: kept, re-labelled.** Deleting a counter that measures a live
comparison would lose coverage; what was wrong was the citation. The mirror
call site and the `decode_path` doc now say in terms that the mirror is not
evidence about `+ss_y` and that the decode path's own reach is
`uv_left_1mi_split_hits()`.

## 8. Comments corrected (all three were unmeasured claims in the source)

* `decode.rs:9780` — "Six committed 4:2:0 fixtures reach it ... and on
  `hg_ss600_key_frame.obu` the boolean flips" -> the measured census of §4.
* `decode.rs:9717` — the `decode_path` justification -> §7.
* `stream.rs:43059` (kf900 gate doc) — "that never happens on a decode-path
  read: `uv_mode_grid` runs down a left mi column always have EVEN length"
  -> the shape is reached; what has never been seen is the two cells
  *disagreeing*.
* `decode.rs:9800` — the DIFF-bucket comment now says which counter is the
  precondition for the other two, so `mode_diff == 0` is no longer readable as
  "unreachable".

## 9. What is still open

A witness needs P(shape) x P(the two blocks disagree on `uv_mode`), and on
today's material the second factor is ~0 where the first is 1-in-2600 reads.
The next lever is not another partition-flag sweep (115 encodes say so) and
not a synthetic source: it is real 4:2:0 material whose 1-mi-tall leaves land
on **chroma** edges — foliage, signage, screen content with saturated chroma —
encoded at a bit depth and cq where the encoder keeps directional chroma
modes. `troy_sb128_inter_witness` (7 reads in 15 frames) is the closest
committed material; the same cut re-encoded at cq 20-32 in 8-bit with
`--enable-1to4-partitions=1` is where to look next. Until one of those
streams lands, the correct statement about this change is the one in §0: the
term matches libaom's source, the geometry it needs is reachable and gated,
and no committed stream yet lets it change a pixel.

### Gaps in this lane's own coverage, stated

* **No 4:4:0 encode.** The term is also live at `ss_x == 0, ss_y == 1`, and
  nothing here produced one: ffmpeg has no `yuv440` pix_fmt and the y4m
  chroma-siting tags do not express it, so the sweep is 4:2:0 only. The
  census in §4 is 4:2:0-only for the same reason (the committed 4:4:4 set
  cannot reach the term by construction). A 4:4:0 hunt needs a raw plane
  writer plus an aomenc input path that accepts it.
* **Three of the five shape-carrying fixtures are not gated** —
  `av112bit-{inter,compound,compound-masked}` carry 1 read each and are
  left out to keep the gate's runtime down; the census in §4 pins them and
  the counter is public, so a future gate can add them for free.
* **No 12-bit-mode sweep of real material.** The three `av112bit` hits are the
  only 12-bit evidence and they are 1 read each.

## State

One commit on `lane-av1leftwit`, nothing pushed. Hunt scratch (115 encodes,
generator, sweep drivers) lives outside the repo in
`~/.cache/hunt-av1leftwit/` and is not part of the commit.
