# lane-av1422late — `s422_384x240`, the one 4:2:2 cell that fails from decode frame 1

**Outcome in one line: localised to the frame-1 alt-ref's inter chroma SKIP write
(`decode.rs:43948`, the single-reference arm's `push_mc_rect(1, cpx, cpy, …)`), and
NOT fixed — the residual is the standing-reserved "group-tail chroma SKIP arm that
writes a flat DC the oracle never predicts", which this lane is forbidden to edit.**

Branch `lane/av1422late`, worktree `/home/tahinli/.cache/wt/av1422late`, base
`main` = `affe70dc`. **The tree carries no source diff** (`git diff --stat` empty;
`stream.rs:1803` reads `if seq.subsampling_x != seq.subsampling_y {`). The
`EC_AV1_ALLOW_422_PROBE` bypass and every temporary trace were reverted before this
commit; the sequence-header refusal is in place. No push.

---

## 0. The cell

| | |
|---|---|
| file | `/home/tahinli/.cache/census422b/sweep/s422_384x240.obu` |
| sha256 | `bf1c658e9bfdc13ec1ed51e59049d2b81201941b6ef4571f8382f356ea4b815b` |
| geometry | 384x240, `subsampling_x=1, subsampling_y=0` (4:2:2), 8-bit |
| coded frame | 384·240 + 2·(192·240) = **184 320 B** |
| decode frames | **17** (16 shown + 1 hidden alt-ref) |

## 1. The comparator, and its liveness proof

`ffmpeg -v error -i <cell> -f rawvideo -pix_fmt yuv422p -` is the pixel oracle.
184 320 B/frame, split by geometry, never by file size.

### 1.1 The trap this cell sets: 17 decode frames, 16 display frames

ffmpeg emits **display** order; our `EC_AV1_FINAL_DUMP` is **decode** order. Running
them index-to-index gives a garbage sheet (frame 1 "18096/11872/12038" wrong). The
full 17x16 ours-x-ffmpeg total-diff matrix
(`/home/tahinli/.cache/late422/align.py`) has its zero band on the anti-diagonal,
which is the decode→display map:

```
decode : 0    1  2  3  4  5  6  7  8  9 10 11 12 13 14 15 16
display: 0  HID  7  3  1  2  5  4  6 11  9  8 10 13 12 14 15
```

**decode frame 1 is the hidden alt-ref** (it has no display counterpart); the shown
frames are reordered around it. Cross-check: aomdec's own `EC_AV1_FINAL_DUMP`
(decode order) agrees with ffmpeg **0/0/0 on every shown frame it can be paired
with**, so the two oracles are the same pixels and only the ordering differs.

### 1.2 Flip control — the comparator bites

One oracle sample flipped (low byte, bit 0) in a frame that is **already wrong**,
chosen as the first *matching* sample so the count must move by exactly +1:

| | Y | U | V |
|---|---|---|---|
| baseline | 0 | **1780** | **1871** |
| one U sample flipped at decode 8 / display 6, U(0,0) `0x5c`→`0x5d` | 0 | **1781** | 1871 |

`/home/tahinli/.cache/late422/aligned.py … flip`. **+1 in that plane of that frame
only, every other plane and frame unchanged.**

## 2. Per-frame, per-plane counts (ffmpeg, on the alignment above)

Wrong **samples** per plane, decode order. `HID` = the alt-ref, no ffmpeg counterpart.

| decode | display | Y | U | V |
|---|---|---|---|---|
| 0 | 0 | 0 | 0 | 0 |
| 1 | HID | (202/210 vs aomdec) | | |
| 2 | 7 | 0 | 50 | 60 |
| 3 | 3 | 0 | 0 | 0 |
| 4 | 1 | 0 | 0 | 0 |
| 5 | 2 | 0 | 0 | 0 |
| 6 | 5 | 0 | 0 | 0 |
| 7 | 4 | 0 | 0 | 0 |
| 8 | 6 | 0 | 4 | 12 |
| 9 | 11 | 0 | 250 | 258 |
| 10 | 9 | 0 | 138 | 150 |
| 11 | 8 | 0 | 96 | 104 |
| 12 | 10 | 0 | 210 | 210 |
| 13 | 13 | 0 | 290 | 303 |
| 14 | 12 | 0 | 292 | 303 |
| 15 | 14 | 0 | 248 | 261 |
| 16 | 15 | 0 | 202 | 210 |
| **total** | | **0** | **1780** | **1871** |

Plus the hidden alt-ref: **0 / 202 / 210** (aomdec decode-order). 11 of 17 decode
frames are wrong — the census's "11/17" reproduced exactly, first at decode 1.

## 3. First divergence

**Decode frame 1 (the alt-ref), chroma only.** Measured against aomdec's
decode-order dump (ffmpeg cannot show a hidden frame):

* plane **U** (and V identically): rows **169..190**, cols **23..35**;
  pre-filter rows **170..189**, cols **23..31**, exactly 180 wrong samples per plane.
* values: **ours is a flat 90** (U) across the whole 9x20 box; the oracle carries a
  vertical profile 87, 98, 120, 131, 128, 128, … 128, 131, 120, 98, 87.

**Classification: a RECONSTRUCTION error, not an entropy desync.** The
pre-filter stage dump (`EC_AV1_PREFILT_WIDE_DUMP` vs aomdec `EC_AV1_PREFILT_DUMP`,
per-frame, decode order) already carries the whole divergence:

```
decode   0     1     2   3-7   8     9    10    11    12    13    14    15    16
Y/U/V  0/0   0/180 0/48  0    0/4  0/246 0/136 0/92 0/202 0/290 0/290 0/248 0/202
```

180 pre-filter → 202 post-filter, so deblock/CDEF/LR only spread the edge. A
desynced entropy decoder cannot produce a byte-exact luma plane for the whole frame
and then fail on one 9x20 chroma box eleven frames later.

## 4. The owning arm

`EC_MCPUSH` (which names the *caller* of every MC/inter write) over the whole
stream: every chroma write landing on the bad box comes from one site —

```
crates/ec-av1/src/decode.rs:43948:21   push_mc_rect(1, cpx, cpy, chroma_stride,
                                        write_chroma_w, write_chroma_h, su, ZERO_RESIDUAL)
```

i.e. **the inter block's chroma SKIP write in `decode_inter_block`'s
single-reference arm** (the compound twin is `decode.rs:42191`). `EC_AV1_PIXPROBE`
confirms the writer at the sample:

```
PIXWRITE mc_add block=(0,128)+32 px=(25,175) prev=90 val=90
```

— a 64x64 luma inter block at (0, 128), chroma plane block (0, 128, 32, 64)
(the per-axis 4:2:2 shape, `decode.rs:40926`), writing a prediction equal to the
reference and adding nothing.

**The reference cannot produce the oracle's samples.** Frame 0's U plane is
**uniformly 90 for columns 0..31 over rows 126..199** (V uniformly 240) — there is
no 128 anywhere near, at any offset. So the oracle's block is *not* a copy of the
reference there, while ours is, byte for byte, over the whole 32x64 chroma plane
block (0 differing samples against frame 0, 180 against the oracle). Our side
produced a **flat DC** exactly where the oracle predicts content: the standing
signature of the reserved arm.

The left edge of the wrong box moves with the object (cols 19,21,23,26,28,30 over
frames 9..16) while the right edge stays at 31..35 and the rows stay 169..190 —
i.e. it is the *chroma footprint of the alt-ref's moving object*, corrupted from
its first appearance and inherited by every later frame that references the alt-ref
(frames 3..7 are exact because they do not).

## 5. Why this lane does not fix it

The arm is inside the class the project ledger reserves:

> the 4:2:0 group-tail chroma SKIP arm — *writes a flat DC the oracle never
> predicts* — is known-broken and reserved for its owning lane; never touch it from
> other chroma work; if another fix or witness depends on it, **stop and report
> instead of editing it**.

This cell's signature is that arm's signature (flat DC in chroma where the oracle
predicts, luma exact, inter block, SKIP). Editing it here would be the recurrent
regression the rule names. **Refusal-to-fix; the owner is the group-tail chroma SKIP
arm's lane.** What is *not* claimed: that the 4:2:0 arm and this 4:2:2 site are the
same line of code — only that the failure mode is the reserved one and the fix must
be made there, in the arm, not here.

Ruled out on the way, with measurements, not inference:
* entropy desync — pre-filter already differs, luma exact (above);
* deblock / CDEF / loop-restoration — pre-filter carries 180 of the 202;
* OBMC — `EC_KILL_OBMC=1` makes the cell strictly worse (frame 13 goes
  0/290/303 → 225/573/577), so OBMC is load-bearing and not the writer;
* compound / warp / scaled-reference — the block reaches neither closure
  (`LATE422P` single-ref parse trace and `LATE422Q` compound parse trace both
  absent for it); `EC_MC_CALL` shows its chroma MC at `xfrac=0 yfrac=0`,
  `hk=Regular vk=Regular`;
* the `EC_AV1_DEBUG_SKIP_*` rungs are not usable for stage attribution on their own
  (they disable the filter on our side only, so every plane goes red); the
  pre/post/post-CDEF **stage dumps** are, and were used instead.

## 6. Family sweep — measured on the same instrument, same tree

`/home/tahinli/.cache/late422/sweep.py` (ours vs aomdec `EC_AV1_FINAL_DUMP`,
decode order, per plane per frame, depth-correct).

| cell | geometry | depth | frames | Y/U/V | verdict |
|---|---|---|---|---|---|
| `s422_384x240` (this cell) | 384x240 | 8 | 17 | **0/1982/2081** | DIVERGES, first at decode 1 |
| `s422_384x242` | 384x242 | 8 | 17 | 0/0/0 | exact |
| `s422_384x246` | 384x246 | 8 | 17 | 0/0/0 | exact |
| `s422_320x240` | 320x240 | 8 | 17 | 0/0/0 | exact |
| `s422_320x246` | 320x246 | 8 | 17 | 0/8339/5903 | DIVERGES (frame 0) |
| `s422_322x246` | 322x246 | 8 | 17 | 0/3603/2324 | DIVERGES (frame 0) |
| `s422_322x240` | 322x240 | 8 | 17 | 0/22969/22534 | DIVERGES (frame 0) |
| `s422_352x242_10b` | 352x242 | 10 | 17 | 0/4858/3949 | DIVERGES (frame 0) |
| `s444_352x242_10b` | 352x242 4:4:4 | 10 | 17 | **0/0/0** | exact — **closed since the census** |

* Every diverging count the census published is **reproduced exactly** on this tree
  (5 of the 5 swept, including this cell's own 0/1982/2081), so the instrument is
  the census's and the numbers are comparable.
* The two "closed at frame 0" geometry siblings of this cell — `s422_384x242` and
  `s422_384x246` — are **byte-exact over all 17 frames**. That is the requested
  "not frame-0-only" control in the only direction available without a fix: the
  384-wide family is not uniformly broken, the defect is reachable only at
  384x240.
* `s444_352x242_10b` (4:4:4) closed between the census's tree and `affe70dc`; the
  census's 0/1166/1413 no longer reproduces. Recorded so the next reader does not
  re-chase it.
* 4:2:0 controls were **not** swept in this run (the driver's y4m header parse does
  not cover the `C420*` spellings aomdec writes) — stated, not assumed.

## 7. Not measured

* No fix, so no after-fix numbers exist; the "gate" this lane adds is the
  localisation itself plus the measurements above. **No new test was added** — a
  gate on an unfixed arm would only pin the defect.
* The oracle's *own* inter chroma prediction is not observable: aomdec's `EC_PRED`
  / `EC_PREDOUT8` rungs fire on the intra path only, so the claim "the oracle's
  block is not a copy of the reference" rests on frame 0's chroma being uniformly
  flat over the whole search neighbourhood, not on a rung.
* Frames 3..7 are exact but the alt-ref is not; the exact propagation path from
  decode 1 to each later bad frame is inferred from the shared geometry, not traced
  reference by reference.
