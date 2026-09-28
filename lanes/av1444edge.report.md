# lane-av1444edge — the two 4:4:4 cells the chroma-format sweep handed over

**Base** `4155c7c7`. **Branch** three commits on top of it, in this order:

| commit | what | owner |
|---|---|---|
| `dc35d607` | the inter-frame pre-filter rung (`EC_AV1_PREFILT_WIDE_DUMP`) did not exist, and its counter restarted — **measurement only, no decode-path edit, merges independently of everything else here** | this lane |
| `e10a3b25` | cherry-pick `-x` of `f92776ba`, lane-av1444rect's H1 fix — **not mine**; see §5 | Levent-2 |
| `a9e5034e` | the gate + the two pinned fixtures | this lane |

Both cells the sweep handed over are **one class, and the fix already
existed on another lane's branch**. The work here is the discriminator
run the sweep said it did not have, plus the rung defect that made the
discriminator run necessary, plus the matrix correction in §6.

---

## 1. The two cells, pinned

Both are the sweep's own §6.2 recipes, byte-reproduced. The truncated
sha256 prefixes the sweep recorded (`c87ac65b…`, `06621606…`) both
match, so these are the same bytes it measured.

| cell | fixture | bytes | sha256 |
|---|---|---|---|
| 4:4:4 odd coded dims 130x122 | `fixtures/444_lossy_rect4_odd_130x122.obu` | 8945 | `c87ac65b77579afd2938669b0cbe3ec44c3abf775c0223cacb9d14b8c2eee27c` |
| "4:4:4 superres" 256x128 | `fixtures/444_lossy_rect4_wide_256x128.obu` | 15239 | `06621606c11ae805f784a1c9736db6301a925a4fa3154e2fc38db1df7ad9b826` |

Recipe (both arms; `W`/`H` = 130/122 and 256/128):

```text
ffmpeg -f lavfi -i "testsrc2=size=<W>x<H>:rate=25" -frames:v 4 \
       -pix_fmt yuv444p -strict -1 -f yuv4mpegpipe - | \
aomenc --codec=av1 --passes=1 --end-usage=q --cq-level=20 --cpu-used=2 \
       --threads=1 --row-mt=0 --lag-in-frames=0 --kf-max-dist=100 \
       --limit=4 --obu -o - -
```

`rate=25` is load-bearing (`testsrc2`'s pattern is time-parameterised).
Both streams: 4 decode-order frames, 0 hidden, single tile, ss (0,0),
8-bit, profile 1, `use_superres=false` on all four frames (see §6).

Measured against the instrumented aomdec (`EC_AV1_FINAL_DUMP`, decode
order), asserting equal frame COUNTS and equal per-frame byte LENGTHS
before any sample is read:

| | 130x122 | 256x128 |
|---|---|---|
| base `4155c7c7` | frames 0-2 **EXACT**, frame 3 11521 samples, first (f3, s2208) = **Y(128,16)** ours 134 / oracle 170 | frames 0-1 **EXACT**, frame 2 25296 first **Y(192,0)** ours 92 / oracle 106, frame 3 51108 first Y(0,0) |

Both numbers are the sweep's, exactly.

---

## 2. The class, and how it was separated from the other candidate

Two candidate fixes were on the table and the two cells are evidence for
exactly one of them.

| arm (each `git reset --hard 4155c7c7` + a verified rebuild) | 130x122 | 256x128 |
|---|---|---|
| base | DIVERGENT f3 11521 | DIVERGENT f2 25296 + f3 51108 |
| base + rung + `4e151813` | DIVERGENT f3 11521 — **byte-identical to base** | DIVERGENT f2 25296 + f3 51108 — **byte-identical** |
| base + rung + `f92776ba` | **EXACT 4/4** | **EXACT 4/4** |
| base + rung + both | EXACT 4/4 | EXACT 4/4 |

**The class is H1, `f92776ba` (lane-av1444rect).** `4e151813` is inert
here, and the code says why rather than leaving it to the numbers: it
patches only the two `tu_reach` call sites in `decode_rect4_16_strip`,
and both sit inside that function's `if lossless_pair {` arm. The lossy
path is the `} else if skip {` at `decode.rs:17579`. These two cells are
4:4:4 **lossy** cq-20, so they are structurally out of its reach.

**Class statement.** `decode_inter_block`'s `around_c` gathered a 1:4
(HORZ_4 / VERT_4) inter strip's chroma coefficient context at the 4:2:0
PAIR extent — `(16, 8)` HORZ / `(8, 16)` VERT — with no `ss` gate. At
4:4:4 `is_chroma_reference` (`av1_common_int.h:1454`) reduces both parity
clauses to `!subsampling_y` / `!subsampling_x`, so every block is its own
chroma reference and no pair exists: the strip codes its own 16x4 chroma
over its own context. The gather read one extra luma mi row of LEFT
chroma context, the U unit's `get_txb_ctx` saw a neighbour libaom does
not have, and the arithmetic coder forked.

What the two new geometries add over the existing H1 gate (128x96): a
partial-frame right edge (130x122 — neither dimension a multiple of 8,
and the sweep's own odd-dimension gate is 4:2:0 only, so the 4:4:4
partial-column walk had no exactness evidence anywhere), and 256x128 with
CDEF and loop restoration both live on the last two frames. 66x66 is
EXACT at the same settings, which is what made the sweep call the
partial-frame walk "right at one odd geometry and wrong at another" — it
is not the geometry, it is whether the stream codes a 1:4 inter strip.

### 2a. A retraction inside this lane

An earlier version of the table above said `4e151813` alone was
sufficient. **It was not, and the arm never ran.** The tree was reset
with `git checkout -- <files>`, which restores from the INDEX — and after
`git cherry-pick -n` the index still held the second pick, so the arm
re-measured the both-commits tree, and cargo saw byte-identical sources
so it did not even relink. The wrong table was sent to two peers, one of
whom had already written it into their report; both were told within the
hour and the retracted version is recorded in their report as retracted.
The table above is the re-run. Kept here because the failure is the
lesson: **the dangerous measurement is the one that returns a number.**
Two agents hit the same class independently the same session (mine via a
stale index, Mustafa-2's via a "fix" worktree that was itself a merge of
two commits).

---

## 3. Localizing, because the discriminator is the reusable part

**The pre-loop-filter reconstruction already diverges at the identical
first sample as the final output**, on both cells (130x122 frame 3 at
Y(128,16); 256x128 frame 2 at Y(192,0)). No filter is involved, so the
deblock/CDEF/LR ladder is not where this lives — and that verdict was
only reachable because of the rung fix in §4. Before it, the nearest
usable rungs were unreadable: `EC_AV1_POSTDEBLOCK_DUMP` /
`EC_AV1_POSTCDEF_DUMP` came out 61440 B against the oracle's 52224 B
(our 160x128 padded surface vs its 136x128 aligned buffer), and the
oracle's POSTDEBLOCK wrote 3 files for a 4-frame stream.

**Order the wrong superblocks by DECODE order, not raster.** Raster order
puts the first wrong sample at Y(192,0) (SBcol3/SBrow0). Decode order
puts it in **SBcol2/SBrow0**, first wrong pixel **(128,48) = mi(12,32)**,
with **zero** wrong pixels in any superblock decoded before it.

**And the block at mi(12,32) has IDENTICAL mode, ref0 and mv0 on both
sides.** The `EC_MODE` ladder (compared per decode-order frame, ours
split by `EC_PICT`, the oracle's by `aomdec --limit`) agrees through its
first 175 entries of frame 2 and then forks on the very next block,
mi(13,32):

```text
AOM  mi_row=12 mi_col=32 mode=13 ref0=1 mv0=(-32,0) stack=1
OUR  mi_row=12 mi_col=32 mode=13 ref0=1 mv0=(-32,0) stack=1     <- same
AOM  mi_row=13 mi_col=32 mode=13 ref0=1 mv0=(-32,0) stack=2
OUR  mi_row=13 mi_col=32 mode=13 ref0=4 mv0=(-32,0) stack=2     <- ref0 LAST vs GOLDEN
```

After the fork the ladder is garbage: 226 entries against the oracle's
299 in frame 2, 40 against 361 in frame 3.

**A wrong pixel cannot cause a mode fork.** Mode context is built from
mode info, not from reconstructed samples. So one bsize/footprint error
has to feed both symptoms, and it is upstream of the pixels rather than a
consequence of them. That argument is what rules out "the reconstruction
diverged, therefore the chroma context followed".

**What the ladder could not have told me:** `EC_MODE`'s print carries no
`bsize`, so a partition or block-size fork is invisible to it by
construction. Nor is its `rng` field a cross-decoder invariant here
(libaom's 16-bit `ec.rng` against a 15-bit `log2` range) — only the
semantic fields compare. The oracle's `EC_PART` rung prints 4x4 mi while
ours prints 16x16-PIXEL units at the sub-node levels, so positions do not
compare either; the read-VALUE sequences do, and over the key frames they
agree. Our `EC_PART` rung is also key-frame-path only, so it says nothing
about these inter frames at all.

---

## 4. The rung fix (`dc35d607`, measurement only)

`EC_AV1_PREFILT_WIDE_DUMP` is the only rung that writes the
pre-loop-filter reconstruction cropped to each plane's mi-ALIGNED extent
— the shape aomdec's rung 7 writes (`y_width`/`y_height`) — so it is the
rung that separates "already wrong in reconstruction" from "introduced by
a loop filter". Two defects, both reporting success:

1. **It was inlined in the key-frame tile path only.** The inter-frame
   path had no wide prefilter dump at all.
2. **Its counter was a function-local `static IDX2`.** Rust gives each
   monomorphisation its own copy, so the index restarted and every frame
   after the first overwrote `.f0` — a per-frame bisection silently
   compared the LAST decoded frame and reported it as frame 0. Measured
   directly: before the change the rung wrote exactly one file for a
   four-frame stream.

Now: a shared `dump_prefilter_wide` called from both tile paths, beside
each path's existing padded `EC_AV1_PREFILT_DUMP` so the two agree on the
point they sample, indexed by the process-global per-var `dump_stage_idx`
map. After the change both cells produce 4 files at exactly the oracle's
byte count. No decode-path behaviour changes: every write stays inside the
existing `if let Ok(path) = crate::envflags::var(...)` arm, which is off
unless the env var is set.

Levent-2 has flagged this to Main as standalone and mergeable
independently. It should not wait on the rest of this lane.

---

## 5. Ownership — this is a handback, not a new fix

The fix is `f92776ba`, cherry-picked here with `-x` so the merge is
idempotent and the branch is green. **Levent-2 owns it.** What this lane
contributes beyond the cherry-pick:

- the discriminator run the sweep said it did not have, and the ablation
  that separates H1 from the other candidate;
- two geometries H1 did not cover, with red-before on both, so the class
  is no longer a single-geometry claim;
- the rung fix, which is what made the discriminator run possible;
- the matrix correction in §6.

The fixtures deliberately sit **next to the gate that fixes them**, not
next to a gate that does not (Mustafa-2's explicit preference, and the
right call: they invite exactly the misattribution §2a records).

---

## 6. Matrix correction: "4:4:4 superres" is not a superres cell

**`--superres-mode=1` without `--superres-denominator` scales nothing.**
The sweep's recipe omitted the denominator, so it was left to the
encoder's energy heuristic — `get_superres_denom_for_qindex`
(`av1/encoder/superres_scale.c:143`) returns `SCALE_NUMERATOR` unless
the frame is a KF/ARF update and the horizontal-energy test passes, and
on a smooth `testsrc2` source it chose 8. Parsed from the stream, all
four frame headers of `444_lossy_rect4_wide_256x128.obu` carry:

```text
FH0 frame=256x128 upscaled=256 use_superres=false denom=8
```

(through `FH3`). The cell was a plain 4:4:4 lossy cq-20 stream wearing a
superres label — the same family as the sweep's own §6.0 trap 2, which
caught `--tile-rows=1` being a no-op at 256x128.

With an explicit denominator the cell is real and **byte-exact 4/4**:

```text
ffmpeg -f lavfi -i "testsrc2=size=256x128:rate=25" -frames:v 4 \
       -pix_fmt yuv444p -strict -1 -f yuv4mpegpipe - | \
aomenc --codec=av1 --passes=1 --end-usage=q --cq-level=20 --cpu-used=2 \
       --threads=1 --row-mt=0 --lag-in-frames=0 --kf-max-dist=100 \
       --limit=4 --superres-mode=1 --superres-denominator=12 \
       --superres-kf-denominator=12 --obu -o - -
```

14808 B, sha256 `450caa3e31526a8973b9f64d5030a2f11b87bd60fac07aef121cb62af9bf85ff`;
all four headers `frame=171x128 upscaled=256 use_superres=true denom=12`;
`predict_scaled_hits=2130`, `scaled leaf8=83 sub8=372`, so scaled MC
really ran. Verdict: **4/4 decode-order frames byte-exact vs aomdec**,
measured on `a9e5034e`'s parent (`dc35d607` + `f92776ba`) with no
4:4:4-specific fix involved beyond H1.

So the sweep's §1 matrix row `4:4:4 / superres / 8-bit` should read
**exact, not D**, and the cell needed a denominator to be a cell at all.
A 36-point sweep over {testsrc2, noise, smptebars} x cq {0,10,20,30} x
denominator {9,12,16} found a scaled 4:4:4 stream in every combination
once the denominator is given.

Not gated: the recipe is a hand-quoted flag set with no pinned fixture
here, and the eight committed superres gates are 4:2:0. Recorded as the
open item rather than closed with a gate I have not mutation-proved.

---

## 7. Gate and its red-before

`a_444_lossy_rect4_strip_stream_decodes_pixel_exact_at_odd_and_wide_geometries`
(`crates/ec-av1/src/stream.rs`). Per arm it asserts:

- the pinned bytes — length and FNV-1a-64 (my FNV implementation was
  validated against the sibling gate's published pair, 6441 B /
  `0x4368_8b2d_2e91_72b0`, before being used on new bytes);
- a non-zero **DELTA** of 1:4 inter strips. `inter16_rect4_counters()` is
  process-wide with no reset, so an absolute count would be a
  non-vacuity assertion that passes on residue from another stream in a
  full-suite run. Measured deltas: 28 and 76.
- `decode::rect4_inter_own_chroma444_hits() > 0` — the corrected route
  demonstrably ran. Measured: 12 and 24.
- the 4:4:4 full-resolution chroma shape (`f.u.len() == w * h`), so the
  compare cannot stand in for the shape claim;
- 4 frames, the claimed dimensions, and every decode-order frame
  byte-exact through `decode_all_frames_vs_oracle`.

**Green** (`--nocapture`, `EC_AV1_REQUIRE_AOMENC=1`, no SKIP line):

```text
a_444_lossy_rect4_strip_stream_decodes_pixel_exact_at_odd_and_wide_geometries:
  fixtures/444_lossy_rect4_odd_130x122.obu   4 decode-order frame(s) (0 hidden)
    byte-exact vs aomdec, 12 own-extent 1:4 chroma gather(s), 28 1:4 inter strip(s)
  fixtures/444_lossy_rect4_wide_256x128.obu 4 decode-order frame(s) (0 hidden)
    byte-exact vs aomdec, 24 own-extent 1:4 chroma gather(s), 76 1:4 inter strip(s)
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 704 filtered out
```

**Red before, by mutation on this gate's own arms.** The `ss_x == 0`
own-extent arm was pointed back at the 4:2:0 `(16, 8)` pair extent, with
the counter left firing so the run goes red on the PIXEL compare and not
on a "counter is zero" assertion:

```text
panicked at crates/ec-av1/src/stream.rs:8469:
a_444_lossy_rect4_strip_stream_decodes_pixel_exact_at_odd_and_wide_geometries:
  decode-order frame 3 of 4 (4 shown, 0 hidden) differs from the oracle at
  byte 4260 (ours 0 vs 38), 9203 bytes differ
test result: FAILED. 0 passed; 1 failed
```

The same mutation measured by hand on both arms: 256x128 goes to 76404
samples with first wrong Y(192,0) — the sweep's exact number — and
130x122 to 9203 with first wrong Y(100,32). Note the mutation changes
the first wrong sample on 130x122, so the base defect and the mutated
defect are not the same fork; the gate is red either way, which is what a
mutation is for.

---

## 8. Identity

All on `a9e5034e`, `--nocapture`, single-threaded, no SKIP except the one
noted.

| family | gates | result |
|---|---|---|
| `a_444*` (both H1 gates among them) | 4 | ok |
| `a_lossless_444*` + `a_real_aomenc_lossless_444*` | 5 | ok |
| superres (`a_superres*`, `a_real_aomenc_*superres*`) | 8 | ok |
| rect-strip (`a_real_aomenc_*rect_strip*`, intra-rect-strip, skipped-lossless-intrabc) | 5 | ok |
| the sweep's three chroma-format gates (cherry-picked `aeb696da` into a throwaway worktree at `a9e5034e`) | 3 | ok |

The three sweep gates, quoted:

```text
a_real_aomenc_odd_coded_dimension_streams_decode_pixel_exact: 6 arms, 4 frames
  byte-exact each, frame-edge walks [0,6,0,4,48,12] [0,0,0,4,0,6] [0,4,0,10,0,10]
  [0,4,0,8,0,12] [0,4,0,8,0,8] [0,0,0,12,0,14]
a_lossless_444_10bit_inter_stream_decodes_pixel_exact: 6 frames byte-exact vs aomdec at 10-bit 4:4:4
a_444_12bit_inter_sequence_decodes_pixel_exact: 6 frames byte-exact vs aomdec at 12-bit 4:4:4
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 705 filtered out
```

**One SKIP, pre-existing and not mine:**
`a_real_aomenc_lossless_444_key_frame_decodes_sample_exact` prints "no
pinned bytes at `fixtures/ll444-lossless-key.obu`". That file has no
entry in `git log` for the path — it was never committed, so the gate has
always skipped on this base.

No suite run beyond the named gates: full validation is Main's, once
after all subagents land.

---

## 9. Handed on

- **Levent-2** — `f92776ba` is the fix, cherry-picked here with `-x`;
  it now has two more geometries and a red-before on each. The
  retraction in §2a is recorded in their report.
- **Mustafa-2** — `4e151813` is confirmed lossless-only by a structural
  argument (`lossless_pair` arm) and by byte-identical divergence counts.
  Independently reached the same conclusion, by a different contaminated
  A/B.
- **Main** — `dc35d607` is standalone, measurement-only, and mergeable
  now. The matrix row `4:4:4 / superres / 8-bit` should read exact (§6).
  Open item: the real 4:4:4 superres cell is exact but ungated.
