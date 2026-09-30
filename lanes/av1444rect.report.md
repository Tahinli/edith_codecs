# lane-av1444rect — H1 (4:4:4 lossy + a 1:4 partition) and the class, round 2

**Tree.** `lane-av1444rect` off `4155c7c7`, worktree `~/.cache/wt/av1444rect`.
Round 1: `f92776ba` (H1 fix + gate + fixture), `bd0d0c37` (report).
Round 2: the class's second instance, `decode_rect4_16_intrabc`, plus a
retraction (§5) and a progress gate (§6). Read §5 before quoting any H3 number
from this report.

## 1. Root cause

`decode_inter_block`'s `around_c` — the gather that decides a 1:4 inter
strip's chroma **coefficient context** — chose the 4:2:0 PAIR extent with no
`ss` gate:

```rust
// crates/ec-av1/src/decode.rs, decode_inter_block
let around_c = match strip_chroma {
    Some(s) if s.has_chroma => {
        let (pw, ph) = if s.horz {
            if ss_x(fctx) == 1 && ss_y(fctx) == 0 { (16, 4) } else { (16, 8) }
        } else { (8, 16) };
        ...
        neighbours.around_mi_rect(s.pair_mi, pw, ph)
    }
    _ => around,
};
```

At ss (0,0) no pair exists. `is_chroma_reference` (av1_common_int.h:1454):

```c
int ref_pos = ((mi_row & 0x01) || !(bh & 0x01) || !subsampling_y) &&
              ((mi_col & 0x01) || !(bw & 0x01) || !subsampling_x);
```

At 4:4:4 `!subsampling_y` and `!subsampling_x` are both true, so `ref_pos`
is **unconditionally true**: every block is its own chroma reference. A
HORZ_4 16x4 strip codes its OWN 16x4 chroma (the `strip_chroma` setter at
`decode.rs:50840` already gives it `pair_mi == at_mi` for exactly this
reason), and its context extent is the strip's own 4 px.

The pair gather used `ph = 8`, which makes `around_mi_rect` walk
`left[mi_r]` **and** `left[mi_r + 1]`. The strip's own 4-px extent walks
`left[mi_r]` only. So the U unit's `get_txb_ctx` OR-ed in a left chroma
neighbour that does not exist, `txb_skip_ctx` came out 1 where libaom's is
offset-7 row 7, and the arithmetic coder forked.

**The fix** is one match arm, gated on the same `ss` the plane geometry is:

```rust
Some(_) if ss_x(fctx) == 0 => { hit!(RECT4_INTER_OWN_CHROMA444_HITS); around }
```

`around` is the block's own `around_mi_rect(at_mi, write_w, write_h)`, which
at ss 0 IS the chroma block's extent. The horz `(16, 4)` sub-branch also
loses its now-redundant `ss_x == 1 &&` conjunct (it is unreachable at
ss_x 0, where the new arm has already returned).

**Class: `ss-gate-missing-on-a-4:2:0-pair-geometry`.** A 4:2:0 pair extent
(or anchor) hardcoded on a path that also runs at ss (0,0), where
`is_chroma_reference` makes the pair non-existent. The 4:2:0 reduction of
the expression is the tell.

### Class sweep (same batch)

| site | verdict |
|---|---|
| `decode_inter_block` `around_c` | **the defect** — fixed |
| `decode_rect4_16_strip` (intra key-frame 1:4), `decode.rs:17308` | already ss-gated (`if ss_x == 0 && ss_y == 0 { (lmi, bw, bh) }`, the `lane-av1-444` row) |
| `decode_rect4_16_intrabc` (`decode.rs:16466`) | **same class, still open** — see §6 |
| `read_inter_rect_chroma`, `decode_block_rect4/64`, the 128-root mu-chunk arm | already ss-parameterised (`uw << ss_x`, `chunk_chroma_w = 64 >> ss_x`) |

## 2. Localization, measured

**Fixture.** `aomenc --codec=av1 --profile=1 --passes=1 --end-usage=q
--cq-level=20 --cpu-used=2 --threads=1 --row-mt=0 --lag-in-frames=0
--kf-max-dist=100 --limit=2` over `ffmpeg -f lavfi -i
"testsrc2=size=128x96:rate=25" -frames:v 2 -pix_fmt yuv444p -f
yuv4mpegpipe`.

6441 B, sha256 `77f727e76d81fe48cd03709f799131677c04869b163e18d78e96bc742ec69153`,
FNV-1a64 `0x04368b2d2e9172b0`. Pinned at
`crates/ec-av1/fixtures/444_lossy_rect4_inter_witness.obu`.

`rate=25` is load-bearing and the sweep report did not say so: `testsrc2`'s
pattern is time-parameterised, so `rate=1` yields a different 8121-byte
stream that decodes exactly. The report's 6441 B / 8884-wrong / Y(91,32)
numbers reproduce only at `rate=25`.

**Red before.** frame 0 exact; frame 1: 8884 of 36864 samples wrong, first at
Y(91,32).

**Symbol fork.** `EC_SYMR` both sides, aligned 1:1 on
`(nsymbs, symbol, post_rng, 32768 - our_icdf0)`: reads 0..26262 pair
exactly, read **26263** diverges, both sides at mi (8,16):

| | site | read |
|---|---|---|
| oracle | `decodetxb.c:158` (the `txb_skip` read) | plane 1 (U), `txb_skip_ctx` **7**, symbol 1 (all-zero) |
| ours | `read_inter_plane_rect` (a 16x4 **chroma** unit) | re-indexed ctx **1**, symbol 0 |

**The report's "ours reads a second LUMA unit" is refuted.** That came from
`read_coeffs_rect`'s `tag=all_zero plane=0` label, which the report itself
flagged as hardcoded. A per-unit trace (`plane`, `w`, `h`, `txb_skip_ctx`,
`around`) shows the block is a **16x4 inter strip** at (64,32) and our
decoder reads exactly the unit the oracle reads — one luma 16x4 at bit 3706,
then one U 16x4 at bit 3719. The fork is a **context row**, not a unit
count. Same conclusion Zeynep-3 reached independently on H5 from the other
half of the same formula.

**Root-of-context, measured** with `EC_DCDUMP` + `EC_CHROMA_PUB`: at the
U unit our gather reported `left[9][plane 1].level = 7` (one extra luma mi
row). The oracle's `get_txb_ctx` there reads `ctx_base 0` →
`txb_skip_ctx = 0 + 7`.

## 3. The fix, measured

| | frame 0 | frame 1 |
|---|---|---|
| before | exact | 8884/36864 wrong, first Y(91,32) |
| after | exact | **byte-exact** |

2/2 frames byte-exact against the instrumented `aomdec`
(`EC_AV1_FINAL_DUMP`, decode order).

**Counter.** `decode::rect4_inter_own_chroma444_hits()` — 9 on the pinned
witness, 0 on the 4:2:0 control of the same source and flags.

## 4. Gate

`a_444_lossy_rect4_inter_stream_decodes_pixel_exact`
(`crates/ec-av1/src/stream.rs`). The first exactness gate for the 4:4:4
**LOSSY** cell — the whole committed 4:4:4 base was lossless 8-bit.

- pinned fixture, length + FNV asserted;
- chroma planes asserted full resolution (the 4:4:4 shape claim, stated
  rather than left to the byte compare to imply);
- byte-for-byte through `decode_all_frames_vs_oracle` (oracle aomdec);
- non-vacuity: the counter must be non-zero, so the corrected route ran;
- specificity: the same source and flags at 4:2:0 code 16 HORZ_4 + 28 VERT_4
  strips (asserted, so the arm cannot pass for the wrong reason), leave the
  counter at 0, and are themselves compared byte-for-byte against aomdec —
  the arm that goes red if the new arm is ever widened past `ss_x == 0`.

**Mutation proof.** Restoring the old pair arm with the counter left in
place:

```
decode-order frame 1 of 2 differs from the oracle at byte 4187
(ours 35 vs 38), 8884 bytes differ
```

— the same 8884 and the same Y(91,32) as the original red, so the gate
catches this defect and not a near-miss of it.

Second direction: widening the arm to `Some(_)` leaves the 4:2:0 control at
0 gathers, because at ss_x 1 a 1:4 strip's chroma reaches
`decode_inter_block` through a different caller and never sets
`inter_strip_chroma`. That is why the identity proof is the 4:2:0 **oracle
compare**, not the counter: the counter proves the 4:4:4 route fired, the
oracle compare proves nothing else moved.

## 5. Identity and the sweep's other divergences

Green against this `decode.rs`:

- the crate's whole `444` family — 9 tests;
- `rect_strip` family — 10 tests;
- `a_real_aomenc_inter_sequence_with_a_16_level_rect_leaf_decodes_pixel_exact`;
- all **three new chroma-format-sweep gates**, run in the sweep worktree
  `~/.cache/wt/av1fmt` with this `decode.rs` copied in and the sweep tree
  restored afterwards:
  `a_lossless_444_10bit_inter_stream_decodes_pixel_exact`,
  `a_444_12bit_inter_sequence_decodes_pixel_exact`,
  `a_real_aomenc_odd_coded_dimension_streams_decode_pixel_exact`.

Same-class recipes re-measured:

| recipe | size | before | after |
|---|---|---|---|
| **H1** 4:4:4 lossy cq20 `--limit=2` | 6441 B | frame 1: 8884 wrong | **byte-exact** |
| H1 control `--enable-rect-partitions=0` | 6529 B | exact | exact |
| **H2** 4:4:4 lossy `--sb-size=64 --min/max-partition-size=64`, 4 f | 21830 B | frames 0-1 exact, from frame 2 608 wrong, U/V only, first U(2,31) | **unchanged** — still 608 wrong, U/V only |
| **H3** 4:4:4 lossless `--tile-columns=1` 256x128, 6 f | 77847 B | sweep reported 90 chroma wrong in frame 0 | **byte-exact, 6/6** |

**H2 is not this class and is not fixed here.** Its entropy decode is
bit-identical over 140820 reads (the sweep measured it), so it is
reconstruction arithmetic, not a context: the `av1_get_adjusted_tx_size`
clamp on a 4:4:4 chroma TX_32X64 is still the prime suspect. Re-measured
here to prove the class claim above: H2's numbers are bit-identical before
and after, so this fix neither caused nor cured it.

> **Derive the length, do not copy the multiplier.** The re-measurement
> below writes `assert len(ours) == 2 * len(ref)`. That factor is a
> **coincidence of 4:4:4 against 4:2:0 at the same geometry** — our `dump_yuv`
> emits `u16` per sample while the oracle's 8-bit dump is `u8`, so the factor is
> the bits-per-sample ratio and has nothing to do with chroma. At 4:2:2, at a
> high bit depth, or on any fixture where the two sides do not agree on bit
> depth, `2 *` is simply wrong. Compute the expected byte length from the plane
> extents and the two bit depths (and assert the oracle's dump length against
> that, not against a constant) before comparing anything.

**H3: RETRACTED — my first claim was void, Selin2-2's measurement stands.**
I reported this stream "byte-exact 6/6". It is not. My throwaway compare
script hardcoded the frame geometry to 128x96 (the H1 fixture's) and
derived the sample count as 3·128·96 = 36864; I fed it a 256×128 witness
without overriding the size, so it compared only the **top-left 36864
samples** of each 98304-sample frame and declared the frame exact. The
defect sits at x ≥ 108, outside the window. A prefix compare passes
exactly when the defect is outside the prefix.

- fixture sha256 `87b0d1a5b3044552fc1fae839e868c30fdc0bebe79f8b3a3104030ffea24ab50`,
  77847 B, `testsrc2 256x128 rate=25` yuv444p, `--limit=6 --kf-max-dist=100
  --threads=1 --row-mt=0 --lag-in-frames=0 --passes=1 --end-usage=q
  --lossless=1 --tile-columns=1 --cpu-used=2` — byte-identical to
  Selin2-2's witness A, so this is a harness bug, not a different-ffmpeg
  story;
- re-measured with the correct geometry on this tip: **63–162 wrong chroma
  samples in every one of the six frames, first at U(110,26)**, luma exact
  — Selin2-2's "29–103 wrong, x ≥ 108, luma exact", reproduced;
- their second witness (75446 B, no `--cpu-used`,
  `65e94e91c56178d21f5c8441877d0dbfbee17bce0916e971fbe6c51677c9891e`)
  reproduces the sweep's original H3 signature (frame 0 only, 12 U + 40 V,
  first U(237,58)) and is bit-identical before and after this fix.

**H3 is not cured by this fix and is not in this lane.** No H3 gate is
pinned here. Selin2-2 owns it. The lesson is in §7: a per-frame byte
compare must assert the two frame lengths match before comparing, and must
not be handed a fixture whose geometry it does not know.

**H5 (lane-av1h5, Zeynep-3): out of scope and structurally disjoint.** The
new arm lives inside `match strip_chroma`, which is `Some` only from the
single setter the PARTITION_HORZ_4 / VERT_4 16x4 / 4x16 `inter_piece` loop
runs; a BLOCK_128X128 unsplit root never reaches it.

## 6. The same class, second instance: `decode_rect4_16_intrabc`

Round 1 left this open on inspection, noting it "needs a 4:4:4 witness I had
no budget to produce". Round 2 produced the witness. **It is live, not
latent.**

**Witness.** `testsrc2 640x480 r=25` yuv444p, `aomenc --tune-content=screen
--enable-intrabc=1 --enable-palette=1 --cq-level=60 --enable-tx-size-search=0
--min-partition-size=4 --max-partition-size=64 --sb-size=64
--enable-rect-partitions=1 --enable-1to4-partitions=1 --cpu-used=0
--kf-max-dist=1 --limit=1 --profile=1` — 1016 B, sha256 `7fe888ed…`, carrying
**17 intrabc 16x4 strips at ss (0,0)**.

**Controls, same recipe each:**

| arm | verdict |
|---|---|
| 4:4:4, intrabc on | 455240 / 921600 wrong, first Y(576,256) |
| 4:4:4, `--enable-intrabc=0` | **byte-exact** |
| 4:2:0 twin, intrabc on (890 B, sha256 `d01352af…`) | **byte-exact** |
| 4:4:4 with `--enable-1to4-partitions=0` and `--enable-rect-partitions=0` | 455240 wrong — the defect does NOT need a 1:4 or rect partition |

So the divergence is intra-BC-at-4:4:4 in general; `decode_rect4_16_intrabc`
is one site in it, not all of it.

**The site.** `decode.rs:16489`: `(pw, ph) = if horz { (16, 8) } else { (8, 16) }`,
`pair_mi = lmi - 1`, `cpx = pair_mi.1 * MI / 2`. At ss (0,0) that predicts and
codes an 8x4 chroma block a quarter of the way off, where libaom codes a 16x4
at the strip's own origin.

**Round 2's change.** One `own444 = ss_x == 0 && ss_y == 0` gate on
`(pw, ph)`, `(cw, ch)`, `pair_mi` and `(cpx, cpy)`. 4:2:0 and 4:2:2 keep every
constant verbatim, so the arm cannot reach them.

**Measured, per plane:**

| | before | after |
|---|---|---|
| first entropy fork | read **11750**, mi (78,128) | read **12465**, mi (90,108) |
| luma first wrong | 164416 (Y 576,256) | **205264** (Y 464,320) |
| luma differing | 95872 | 51716 |
| U first wrong | 20532 | 20532 (untouched) |
| V first wrong | 20532 | 20532 (untouched) |
| total differing | 455240 | 390380 |

**A partial improvement, NOT a fix, and the report says so.** The ss gate
moves the luma divergence 40948 samples later and the entropy fork 715 reads
later; both chroma planes are untouched. 390380 of 921600 samples are still
wrong. No exactness is claimed — "the divergence moved" is evidence that the
geometry was wrong, not that the new geometry is right.

### Gate

`a_444_intrabc_rect4_reads_its_own_chroma_plane_block` — deliberately **not**
an exactness gate. It asserts nothing about the 4:4:4 pixels:

- both fixtures pinned (length + FNV);
- non-vacuity: `intrabc_rect4_own_chroma444_hits()` is non-zero on the 4:4:4
  witness (18) and zero on the 4:2:0 twin, so the route the ss gate sits on is
  proven to run and proven to be ss-specific;
- identity: the 4:2:0 twin is compared byte-for-byte through
  `decode_all_frames_vs_oracle`, which asserts the two frame lengths match
  before comparing;
- the open chroma divergence — 390380 of 921600 wrong, U and V both first wrong
  at index 20532, entropy fork read 12465 at mi (90,108) — is named in a comment
  and in the test's output line, and **never asserted**.

**A prefix-floor assert was written here first and removed.** It asserted the
luma plane is byte-exact to sample 205264 — i.e. that our output is wrong by
exactly N past that point. That encodes a defect as expected behaviour, the
shape this suite refuses everywhere else, and it is a red-if-fixed assertion.
Removed rather than kept as a regression guard. The red-before for the ss gate
itself was measured instead, as an entropy measurement: `own444 = false` puts
the first fork back at read 11750 / luma sample 164416, and 12465 / 205264
with it.

### Round 3 — the chroma root is narrowed, not landed

The follow-up named two candidates; **measurement eliminates both**:

| candidate | verdict |
|---|---|
| (a) `sub8_leaf_chroma444`'s single `around_mi_rect(lmi, bw, bh)` reused for both planes | its `square` arm reads through `read_plane`, and a per-unit trace of every coefficient entry point (`read_plane`, `read_coeffs`, `read_coeffs_rect`) shows **no such unit at the fork's bit position** |
| (b) the non-lossless rect arm's offset | that arm's `read_coeffs_rect` calls fire at msac bits 1912–2097, nowhere near the fork |

**The fork, localized to a call shape.** Instrumenting all three coefficient
entry points pins read 12465 to a `read_coeffs_rect` call with **w=4, h=8,
`skip_ctx`=0** at bit 6893, at mi (90,108) — a 4x8 rect unit where the oracle is
on a chroma `txb_skip` (`decodetxb.c:158`, symbol 1). Which caller and which
plane is not yet established.

### Round 4 — the unit census: it is NOT an ordering defect

Main's neighbour lanes both warned that a chroma-only, ordering-suspect defect
should be checked by COUNTING units first (Cem-2's H5 was a token-order
blowup, 16x too many chroma walks). Counted on the 1016 B witness, one frame:

| | ours | oracle |
|---|---|---|
| total coefficient unit reads | 2688 | 2395 |
| luma (square reader) | 1213 | 1309 |
| chroma (square reader) | 636 | 543 per plane, 1086 total |
| rect-unit reads (`read_coeffs_rect`) | 839 | (subsumed above) |

The oracle's 2395 `EC_COEFF_STEP tag=all_zero` lines are per
`av1_read_coeffs_txb` call and are NOT skipped on eob=0, so the two sides are
counting the same thing. The counts are in the same range — no 16x blowup, no
missing order. **The intra-BC 4:4:4 chroma defect is not a token-order
defect**, which is what Main asked to rule out before chasing a value-level
root, and it is the second independent lane to reach that conclusion about a
4:4:4 chroma fork (Cem-2's own 128-root turn, Selin2-2's H2/H3 census).

There IS an asymmetry worth recording for whoever continues: we read 636
square chroma units against the oracle's 1086, and 2052 luma reads against its
1309. The luma side is pixel-exact to sample 205264, so the luma surplus is
most likely a counting-shape difference (our var-tx leaves versus libaom's
unit walk) rather than a walk defect; it is NOT established either way, and I
am not claiming it.

### Round 6 — the caller is NAMED

Main's ask was to name the caller of the `read_coeffs_rect(4, 8, skip_ctx=0)`
shape. Done with `#[track_caller]` on `read_coeffs_rect` and on
`read_inter_plane_rect`, which turns every call into a file:line tag with no
backtrace and no per-site edit. One run, exact answer:

| bit | call site | plane | x, y | w x h | around |
|---|---|---|---|---|---|
| 6885 | `decode.rs:13165` | 0 | 416, 368 | 8x16 | (0,0,0) |
| **6893 (the fork)** | **`decode.rs:13184`** | **1 (U)** | **208, 184** | **4x8** | **(0,0,0)** |
| 6902 | `decode.rs:13202` | 2 (V) | 208, 184 | 4x8 | (0,0,0) |

All three are inside **`decode_intrabc_rect`** (`decode.rs:12955`), its
non-lossless whole-block rect arm — the `read_inter_plane_rect` triple for
luma then U then V off one shared
`around_mi_rect((mi_r, mi_c), bw, bh)` gather. The luma unit is 8x16, the
chroma units 4x8, which at ss (0,0) means a **4x8 chroma unit under a 4x16
luma plane block** — the sub-8 unit walk, which is exactly the prior Selin2-2's
census established and exactly the class shape.

So the third instance of this lane's class is not a pair extent at all in its
outward form: it is one gather, taken at the LUMA footprint, serving a CHROMA
unit whose own extent is half that on the long axis. At 4:2:0 the two coincide
after subsampling, which is why the 4:2:0 twin is byte-exact; at 4:4:4 they do
not.

**A second observation that is NOT yet established and I am not claiming:** at
this same bit the oracle reports mi (90,108) — luma px (360,432) — while our
chroma unit is at px (208,184), mi (46,52). Every read before this one paired
exactly, so both decoders are at the same bit position after the same number of
reads, which is hard to reconcile with a different block. Either the oracle's
`xd->mi_row/mi_col` is not naming the same block our `x, y` name, or there is a
position divergence I have not localised. Not established; recorded so the next
lane does not have to rediscover the discrepancy.

**Handed back, not landed.** The remaining 4:4:4 intra-BC defect is a **4x8
rect coefficient unit**, present with 1:4 and rect partitions both disabled,
and it is neither of the two candidates the follow-up named. Landing a partial
is out of scope for this round, so the merge candidate is the H1 fix (exact,
gated, mutation-proven) plus the measured `own444` gate on the intra-BC 1:4
geometry, and the chroma root goes on as a fresh task with the numbers above.

**The fastest next measurement** (Selin2-2, from the H2 round): the per-unit
`txtype` census from the oracle's `idct.c` rung. On H2 it found the fault in
one pass — two chroma units whose `txtype` differed from what the block-level
type implied, on a frame where every luma unit was IDTX, so "luma is exact"
carried no information. This cell has exactly that asymmetry (entropy fork on
a chroma unit, luma clean), so the census decides in about a minute whether
the fork is a coefficient-read problem or a type/extent problem — the only
fork left in this cell.

## 7. Residue handed on, not fixed here

- **4:4:4 intra-BC chroma** — a 4x8 rect coefficient unit. CALLER NAMED in
  round 6: `decode_intrabc_rect` (`decode.rs:12955`), the non-lossless
  whole-block rect arm, at `decode.rs:13184` (U) and `13202` (V). One gather,
  `around_mi_rect((mi_r, mi_c), bw, bh)`, taken at the LUMA footprint and
  serving a 4x8 chroma unit under a 4x16 luma plane block; at 4:2:0 the two
  coincide after subsampling (hence the exact 4:2:0 twin) and at 4:4:4 they do
  not. Fork read 12465, bit 6893, U and V first wrong at index 20532. Both
  originally-named candidates and the token-order hypothesis remain dead by
  measurement. Unexplained and unclaimed: at that bit the oracle reports mi
  (90,108) while our unit is at px (208,184) — see §6.
- **H2** and **H3** are with Kaan-2 as of this writing (Selin2-2's round
  closed with the census committed); §5's retraction is the record they need.
- **H4** — 4:4:4 lossless + tile rows, hard divergence. Own lane; untouched.
- **H5** — 4:4:4 10-bit lossless 128 root, chroma band/offset term. Zeynep-3.
- **H6** — 12-bit 4:4:4 needs a smoothed source. Coverage caveat, not a defect.

**The prior, adopted from Selin2-2's round 4.** 4:4:4 chroma defects in this
decoder cluster in the **sub-8 and strip/partition unit walks, not in the
64x64 whole-block paths**. The 64x64 whole-block 4:4:4 paths are exact in every
cell closed so far; the odd-dimension 66x66 control is exact; 4:2:0 12-bit
tiles and superres are exact. Everything divergent is a 4:4:4 cell whose
chroma is split into units smaller than the block, or where a partial-frame
geometry applies. It matches what is left here — a 4x8 rect unit, not a strip
— and it reframes the earlier "it is the 1:4 reader" guess as wrong.

The sharper frame is Selin2-2's: **a value whose scope is wrong at the point
it is consumed.** Both candidates this lane chased were that mistake, just not
at the site — one had a per-plane extent that was not at the fork at all, the
other fired 4800 msac bits away.

## 8. Scope confirmation (Huseyin-2's ablation — FIRST VERSION RETRACTED)

**This section was published wrong and is corrected here.** The first version
of it recorded a four-arm ablation in which `4e151813` alone was EXACT 4/4 on
both of Huseyin-2's cells, concluding the two fixes were independent and that
this lane's fix did not reach them. That arm never ran: the tree was reset with
`git checkout -- <files>`, which restores from the INDEX, and the index still
held both cherry-picks — so that arm silently re-measured the both-commits
tree, and cargo saw byte-identical sources so it did not even relink. The
conclusion was a phantom. **The claim "this lane's fix does not reach those two
cells" is retracted; the opposite is true.**

The corrected 2x2, re-run by Huseyin-2 with `git reset --hard 4155c7c7` and a
verified rebuild (binary mtime printed) at every arm, on their pinned fixtures,
4/4 decode-order frames vs aomdec:

| arm | odd444_130x122 | sr444 (256x128) |
|---|---|---|
| base | DIVERGENT f3 11521 | DIVERGENT f2 25296 + f3 51108 |
| base + rung + `4e151813` | DIVERGENT f3 11521 | DIVERGENT f2 25296 + f3 51108 — byte-identical to base |
| base + rung + `f92776ba` (this lane) | **EXACT 4/4** | **EXACT 4/4** |
| base + rung + `4e151813` + `f92776ba` | EXACT 4/4 | EXACT 4/4 |

Three consequences, all of which enlarge this lane's scope rather than narrow
it:

1. **Both cells are this class, and `f92776ba` alone fixes them.** `4e151813`
   contributes nothing to either: the divergence counts are identical to base
   with and without it, because its two `tu_reach` call sites both sit inside
   `decode_rect4_16_strip`'s `if lossless_pair {` arm while both cells are
   4:4:4 LOSSY cq-20 on the `} else if skip {` path at `decode.rs:17579`.
2. **`f92776ba` does not depend on `4e151813`.** It applies and goes green on
   `4155c7c7` on its own, so merge order is free and nothing else is a
   prerequisite.
3. **Red-before on both cells**, with the counter left in place: mutating the
   `Some(_) if ss_x(fctx) == 0 => around` arm back to the 4:2:0 pair extent
   `(16, 8)` gives sr444 76404 samples with first wrong Y(192,0) — the sweep's
   exact number — and odd444_130x122 9203 samples, first wrong Y(100,32) on
   frame 3.

So the 4:4:4 lossy class now has two more independent witnesses at geometries
and depths this lane's 128x96 gate does not cover (130x122, a partial-frame
right edge; 256x128, with CDEF and LR live on the last two frames). One
correction to the sweep's own matrix, from Huseyin-2's report: its "4:4:4
superres" cell turns out not to be a superres cell at all, and that one is
EXACT.

**Method notes carried over** (still valid; the retraction was in the ablation,
not in the instrumentation):

- The first wrong SYMBOL is invisible in the `EC_MODE` ladder because that
  print carries no `bsize`. What localizes these cells is the per-superblock
  decode-order pixel map of the pre-filter dump — the first wrong superblock in
  decode order is the one whose above-neighbour context then forks the NEXT
  block's `ref0`. On sr444 f2 that is SBcol2/SBrow0, first wrong pixel (128,48)
  = mi(12,32) with the mode identical on both sides, and the `ref0` fork (LAST
  vs our GOLDEN) lands on the very next block, mi(13,32). A pixel error cannot
  cause a mode fork, so the same bsize/footprint error has to feed both.
- **`git checkout -- <files>` does not reset a tree to a commit.** It restores
  from the INDEX, so any staged cherry-pick survives it and the "arm" measures
  the tree you thought you had removed. A per-arm ablation needs
  `git reset --hard <base>` and a rebuild whose binary mtime is printed, or it
  measures nothing. This bit me twice in one session: here, and earlier when a
  temporary trace landed in the main checkout instead of the lane worktree
  because the edit tool resolved a relative path against the workspace root.

## 9. Method notes for the next lane

- **A route counter is not proof a fix bites.** This lane learned it the hard
  way: a counter can fire identically before and after a change that changes
  nothing downstream, so a non-zero count proves REACH and nothing more. Both
  counters this lane added carry that warning in their own doc comment, and
  the intra-BC one is deliberately armed OUTSIDE the `own444` expression so the
  mutation still increments it and the red lands on the pixel/entropy arm
  instead. What carries the claim is the measured fork move and the mutation
  red. Rule now in skill://ec-av1-pipeline-gate-counters: a counter needs a
  DIFFERS-style assertion before it can carry a claim.
- `testsrc2` is time-parameterised: any sweep recipe quoting it must pin
  `rate`, or the fixture is not reproducible. `rate=1` vs `rate=25` on
  128x96 yuv444p is the difference between an 8121-byte exact stream and
  the 6441-byte divergent one.
- `read_coeffs_rect`'s `EC_COEFF_STEP tag=all_zero` prints a hardcoded
  `plane=0`. It has now mis-attributed a chroma read as a luma one in two
  independent reports. The oracle's `EC_COEFF_STEP` prints the real plane.
  Pair the two traces by the msac `post_rng` (both sides print it after the
  read) rather than by line index.
- `EC_AV1_TELL ... label=block_entry` prints the msac bit position at each
  `decode_inter_block` entry, and the oracle's `EC_SYMR` `pre` third field
  is our bit position minus a constant 15. That pair joins the two symbol
  streams to a block without any new instrumentation.
- `EC_DCDUMP` already dumps the above/left entropy contexts per plane;
  `#[track_caller]` on `Neighbours::around_mi_rect` names the gather site
  in one line if you ever need to attribute one again.
- **A byte compare that truncates to `min(len(ours), len(ref))` is a PREFIX
  compare, and a prefix compare passes exactly when the defect is outside the
  prefix.** This lane reported an H3 stream "byte-exact 6/6" that way: the
  throwaway script carried the H1 fixture's 128x96 geometry, so it compared
  36864 of 98304 samples and the divergence at x >= 108 was invisible. Selin2-2
  caught it. **Every ours-vs-oracle compare must assert the two lengths are
  equal before it compares anything**, must not be handed a fixture whose
  geometry it does not know, and must clear BOTH sides' scratch files between
  runs (mine cleared `aom.*` and not `ours.*`, a stale-file hazard sitting right
  next to the truncation bug). The crate's own
  `decode_all_frames_vs_oracle` does assert `got.len() != want.len()`, which is
  why the committed gates in this lane were never exposed to it.
