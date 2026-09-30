# lane-av1422luma — `s422_416x250_10b` is BYTE-EXACT: the luma desync is a 4:2:2 chroma-ORIGIN error in the IntraBC 1:4 strip arm

**Outcome in one line: the one 4:2:2 cell whose luma was also wrong was an
ENTROPY desync, not a luma defect — `decode_rect4_16_intrabc` reads a 16x4
intra-BC strip's chroma at 4:2:0's pair origin at ss (1,0), the wrong
`txb_skip` context makes `all_zero` decode as 1 instead of 0, and the frame
desyncs from that read onward; giving the strip its own chroma at 4:2:2
(own origin, own extent, own 4:2:2 gather) closes SIX sweep cells including
this one, 25 of 26 swept cells byte-exact, 4:2:0/4:4:4 untouched.**

Tip: branch `lane/av1422luma`, base `1bc6f543` (`main` at charter time),
fix commit `459e425d`. Worktree `/home/tahinli/.cache/wt/av1422luma`.
Ours = `examples/decode_probe`, release, private
`CARGO_TARGET_DIR=/home/tahinli/.cache/tgt/av1422luma`, with the
local-only `EC_AV1_ALLOW_422_PROBE` bypass applied **in a separate detached
worktree** (`/home/tahinli/.cache/wt/av1422luma-probe`) by a patch-run-restore
script. **The shipped tree refuses 4:2:2 by name and still does**:
`stream.rs:1803` reads `if seq.subsampling_x != seq.subsampling_y {` in the
committed tree, `grep -c 'if false &&' crates/ec-av1/src/stream.rs` = 0.

Reproducer: `/home/tahinli/.cache/census422b/sweep/s422_416x250_10b.obu`,
sha256 `d6aa7f450f54d6fb381400a0a0226da87599c00509eeb5031abe63383e272627`,
416x250, ss (1,0), 10-bit, 17 decode frames (16 shown + 1 hidden alt-ref),
encoded by the census's `mksweep.sh` recipe (`aomenc --profile=2
--input-bit-depth=10 --limit=16 --lag-in-frames=25 --auto-alt-ref=1
--enable-global-motion=1 --pass=1 --cq-level=24 --threads=4 --kf-min-dist=0
--kf-max-dist=999999 --cpu-used=0 --obu`).

---

## 1. Oracle, instrument, and the liveness proof

Pixel oracle: **ffmpeg 8.1.3** (`libdav1d`), `ffmpeg -v error -i <cell>.obu
-f rawvideo -pix_fmt yuv422p10le -`, 16 shown frames of 416000 B each
(416x250 + 2x208x250, 2 B/sample). Geometry is an explicit argument to the
comparator, parsed from `ffprobe` (`width=416 height=250
pix_fmt=yuv422p10le`), never from a file size; a frame whose length disagrees
with the geometry is a hard error. A wrong SAMPLE is one 2-byte unit at
10-bit, so the top bits cannot hide. A count over zero frames is a hard
error, so a vacuous 0/0/0 cannot be reported.

**Flip controls (the comparator is shown to bite before any "exact" is
read).** One oracle sample's low byte bit 0 flipped per plane, in a named
frame:

| cell | build | baseline Y/U/V | flip | result |
|---|---|---|---|---|
| `s422_416x250_10b` | post-fix | 0/0/0 | frame 0 Y (x=10,y=5) | **Y 0→1**, U 0, V 0, first-Y = the flipped sample |
| `s422_416x250_10b` | post-fix | 0/0/0 | frame 0 V chroma (x=10,y=96) | **V 0→1**, U 0, Y 0, first-V = the flipped sample |
| `s422_416x242_10b` | post-fix | 0/0/0 | frame 0 U chroma (x=168,y=100) | **U 0→1**, Y 0, V 0 |
| `s422_416x242_10b` | post-fix | 0/0/0 | frame 0 V chroma (x=168,y=100) | **V 0→1**, Y 0, U 0 |

Each arm moves exactly one plane by exactly +1 and leaves the other two and
all other frames alone.

A second, independent oracle is used for the hidden alt-ref frame ffmpeg
never presents: the instrumented libaom
(`/home/tahinli/.cache/aom-oracle/build/aomdec`) with
`EC_AV1_FINAL_DUMP`, i.e. **DECODE order**, 17 frames, geometry explicit,
compared with the census's own `cmp422.compare`. Post-fix,
`s422_416x250_10b`, `s422_416x242_10b` and `s422_320x246` are **17/17
byte-exact** in that view too (§5).

## 2. The first divergence, and that it is ENTROPY, not reconstruction

Pre-fix, per shown frame (`s422_416x250_10b`, ffmpeg oracle):

```
frame 0: Y=6538 U=3527 V=3514   first Y frame 0 (x=384,y=128) ours=370 orc=371
                                first U frame 0 chroma (x=168,y=100) ours=830 orc=810
                                first V frame 0 chroma (x=168,y=100) ours=963 orc=897
```

Two filters were INERT, so the difference is already in reconstruction and
not introduced by a post-reconstruction filter:

| stage (ours, depth-correct `*_DUMP16`) | frame 0 Y wrong vs ffmpeg |
|---|---|
| `EC_AV1_PREFILT_DUMP16` (pre-loop-filter) | 6538, first (384,128) |
| `EC_AV1_POSTDEBLOCK_DUMP16` | 6538, first (384,128) |
| `EC_AV1_POSTCDEF_DUMP16` | 6538, first (384,128) |
| `EC_AV1_FINAL_DUMP` | 6538, first (384,128) |

The wrong luma region is a staircase inside superblock row 1 (rows 128-249,
cols 320..415 growing leftward as rows increase; per-8x8-block wrong counts
63-64 of 64 over most of it), with constant per-block offsets (`+131`,
`+2`, `+27`, `-53`, `+1`) in the flat blocks and growing errors elsewhere —
the profile of a wrong value fed into intra prediction, not of a wrong
filter.

**But it is a PARSE desync, and the ladders say so.** Paired against the
instrumented aomdec:

1. `EC_TRACE_MODE=1` intra ladder (`EC_IMODE`/`EC_IMODE_VAL`, per block,
   `rng` = msac state): the first **450** blocks of frame 0 pair
   **element-for-element on `(mi_row, mi_col, rng)`**. The first mismatch is
   index 450, at **mi (52,80)** = luma (320,208): the oracle reads a 16x16
   block, we read 8x16, `rng` 42970 vs 56224.
2. `EC_TRACE=1` partition ladder (`EC_PART`, which also prints the bit
   position): for the first 367 partition reads of frame 0 our `rng` equals
   the oracle's exactly and our bit position is a constant +15 (a tell
   convention offset). At the 16x16 read of **mi (52,80)** the oracle is
   `ctx=5 value=0` and we are `ctx=1 value=2`; our bit position has moved to
   +5, i.e. **we consumed 10 fewer bits than the oracle inside the block
   before it** (mi (51,84), a 16x4 intra-BC strip).
3. Per-TU ladder (a local `EC_TU` rung added in the probe worktree only,
   mirroring the oracle's `EC_ECDUMP_IN`, which prints plane, mi, tx, ctx
   and the exact bit position): through TU 1918 the bit offset is the
   constant +15 and the rng matches at every TU. **TU 1919 — the V-plane
   transform unit of the intra-BC block at mi (51,84) — is the first
   divergence**: oracle `bit=46696 rng=45064`, ours `bit=46701 rng=53200`.
   The bit position still matches (offset +5 after the drop from +15), so
   this is not a bit-position slip: the **Coder STATE** differs at the same
   bit, which is the signature of a wrong CDF row on the read that precedes
   it.
4. The preceding read is the U-plane unit of the same block. The oracle
   reads `all_zero=0` (25 bits, `eob=8`, `tx=6` = TX_8X4); we read
   `all_zero=1` and read no coefficients at all. The oracle's
   `EC_ECDUMP` for that unit is `plane=1 mi=(51,84) bc=0 br=0 tx=6 ctx=8
   above=[23,23,] left=[0,]`; our `EC_RCT` rung for the same unit is
   `skip_ctx=2 around=(true, true, 5)`. libaom's chroma skip context is
   `combine_entropy_contexts(above, left) + 7` = 1 + 7 = **8** (above
   non-skip, left skip); ours is 1 + 1 = **2**, i.e. **our LEFT flag was
   wrong**, because we gathered it one mi row higher than the unit is.

**Verdict: an entropy desync whose first wrong read is the chroma
`txb_skip` of an intra-BC 16x4 strip — a chroma-plane error that corrupts
luma because the desync runs to the end of the frame.** This is why this is
the one 4:2:2 cell with luma wrong: in the chroma-only cells the same
misplacement happens to leave `all_zero` at 1 on both sides, so the parse
survives and only the reconstruction is wrong.

## 3. The owning arm, and the fix

`crates/ec-av1/src/decode.rs`, `decode_rect4_16_intrabc` (the intra-BC 1:4
strip: 16x4 HORZ / 4x16 VERT).

A 16x4 luma strip's chroma plane block is **its own** at 4:2:2, not 4:2:0's
pair: 4:2:2 subsamples x only, so the chroma is 8x4 — full height — at the
strip's own origin, and libaom's own clause agrees
(`av1_common_int.h:1454`, `is_chroma_reference` ends in
`... || !subsampling_y`, true for every block at ss_y 0). The function gated
that whole geometry on ss (0,0) and left 4:2:2 on the 4:2:0 constants
(`decode.rs:18243` before the change said so in as many words: *"4:2:0 and
4:2:2 keep every constant below verbatim"*). Reach in the reproducer alone:
**56 HORZ 1:4 strips in frame 0** (56 `EC_IMODE ... bsize=17` lines).

Four sites move together, all gated on the new
`own422 = ss_x==1 && ss_y==0 && horz`:

| site | before | after (4:2:2) | whose |
|---|---|---|---|
| `cw, ch` | `(pw/2, ph/2)` = (8,4) | `(bw>>ss_x, bh>>ss_y)` = (8,4) | av1422seed |
| `pair_mi` | `(lmi.0-1, lmi.1)` | `lmi` | av1422seed |
| `cpx, cpy` | `(pair_mi.1*MI/2, pair_mi.0*MI/2)` | `(px>>ss_x, py>>ss_y)` | av1422seed |
| chroma context gather (x2) | `around_mi_rect(pair_mi, pw, ph)` | `around_mi_422_chroma(lmi, bw, bh)` | **this lane** |
| uv-mode + left/above publication | `(pair_mi, pw, ph)` | `(lmi, bw, bh)` | **this lane** |

`cw,ch` was already accidentally right at (8,4) (that is what the report
`lanes/av1422seed.report.md` §3.1 says); the origin and the gather were not,
and the origin is what put the unit a mi row high.

**Ownership, stated because it matters for the merge:** the first three rows
are `lane/av1422seed`'s hunk. It was NOT in the tree when this lane started
(`git diff main lane/av1422seed` = empty; only the report had landed), and
that lane's own measurement of the write-side half alone REGRESSED luma
(`0 -> 5958` wrong luma samples on `s422_320x246`) because the gather stayed
on the pair extent — which is exactly the split this report's §2 measures.
Reproducing the origin half here is what makes the arm measurable whole and
is what closes their five cells plus mine. **Derya-3 was messaged before
either hunk was written; do not land a second copy of the origin hunk.**

Deliberately NOT changed: VERT_4 stays on the pair
(`ss_size_lookup[BLOCK_4X16][1][0]` is `BLOCK_INVALID` at 4:2:2, so a 4x16
strip is not codable there); 4:2:0 is untouched by construction (every new
branch is `own422`-gated); 4:4:4 keeps the **per-mi** gather, because at
ss_x 0 a chroma column IS a luma mi column and `around_mi_422_chroma`'s
every-second-column sampling would halve its own above span (measured: the
4:4:4 witness's first-wrong index and entropy fork are unmoved, §4).

## 4. Gate

`a_444_intrabc_rect4_reads_its_own_chroma_plane_block` (`stream.rs:9586`)
passes, and its printed numbers are **identical before and after** the
change: `23 own-chroma intra-BC strip(s) at 4:4:4, 0 at 4:2:0; 4:2:0 twin
byte-exact; 4:4:4 intra-BC CHROMA still OPEN (U/V first wrong at index
20532, entropy fork read 12465 at mi (90,108))`. That is the 4:2:0/4:4:4
control for this hunk: the route counter is unchanged at 4:4:4, the 4:2:0
twin of the same recipe is byte-exact against the oracle, and the 4:4:4
intra-BC chroma residual has not moved. The 26 `intrabc`-named tests pass
(1 pre-existing `#[ignore]`).

**What cannot be gated, said plainly:** 4:2:2 is refused at the sequence
header, so no committed fixture or test can reach this route — a
pixel assertion for it is not expressible today. The honest package is the
counter `INTRABC_RECT4_OWN_CHROMA422_HITS` /
`intrabc_rect4_own_chroma422_hits()` (reach only; same caveat as its 4:4:4
twin, and it needs a bypassed build to read), plus the measurement below. A
source-scan substitute is not a gate and is not offered as one.

## 5. Measurement

Instrument: `/home/tahinli/.cache/w422luma/sweep.sh` + `cmp2.py` (per-plane,
per-SHOWN-frame wrong-sample counts and first divergence, geometry explicit,
ffmpeg 8.1.3 oracle) and `cmp_decode_order.py` (the census's
`cmp422.compare` over `EC_AV1_FINAL_DUMP` on both sides, DECODE order).
`pre` = `main` 1bc6f543 with the same bypass, built into a separate target
dir; `post` = this tree, same bypass, same comparator, same cells.
8-bit cells go through `EC_PROBE_OUT` (8-bit planes) and 10-bit through
`EC_PROBE_OUT16` (u16 LE) — mixing them is the instrument bug that once
reported 1.2 M wrong luma samples on a cell that is exact.

### 5.1 Whole sweep, one line per cell (16 shown frames, wrong samples Y/U/V)

| cell | geometry | pre | post |
|---|---|---|---|
| `s422_320x240` | 320x240 8b | 0/0/0 | 0/0/0 |
| `s422_320x242` | 320x242 8b | 0/0/0 | 0/0/0 |
| `s422_320x246` | 320x246 8b | 0/7810/5531 | **0/0/0** |
| `s422_322x240` | 322x240 8b | 0/22003/21625 | **0/0/0** |
| `s422_322x242` | 322x242 8b | 0/0/0 | 0/0/0 |
| `s422_322x246` | 322x246 8b | 0/3437/2222 | **0/0/0** |
| `s422_326x240` | 326x240 8b | 0/0/0 | 0/0/0 |
| `s422_326x242` | 326x242 8b | 0/0/0 | 0/0/0 |
| `s422_326x246` | 326x246 8b | 0/0/0 | 0/0/0 |
| `s422_384x240` | 384x240 8b | 0/1780/1871 | 0/1780/1871 (unchanged, §6) |
| `s422_384x242` | 384x242 8b | 0/0/0 | 0/0/0 |
| `s422_384x246` | 384x246 8b | 0/0/0 | 0/0/0 |
| `s422_320x242_10b` | 320x242 10b | 0/0/0 | 0/0/0 |
| `s422_320x250_10b` | 320x250 10b | 0/0/0 | 0/0/0 |
| `s422_352x242_10b` | 352x242 10b | 0/4653/3777 | **0/0/0** |
| `s422_352x250_10b` | 352x250 10b | 0/0/0 | 0/0/0 |
| `s422_416x242_10b` | 416x242 10b | 0/5740/4348 | **0/0/0** |
| **`s422_416x250_10b`** | **416x250 10b** | **99011/61486/60106** | **0/0/0** |
| `s420_416x250_10b` | 416x250 10b 4:2:0 | 0/0/0 | 0/0/0 |
| `s420_416x242_10b` | 416x242 10b 4:2:0 | 0/0/0 | 0/0/0 |
| `s420_352x242_10b` | 352x242 10b 4:2:0 | 0/0/0 | 0/0/0 |
| `s420_320x242_10b` | 320x242 10b 4:2:0 | 0/0/0 | 0/0/0 |
| `s420_320x250_10b` | 320x250 10b 4:2:0 | 0/0/0 | 0/0/0 |
| `s444_416x250_10b` | 416x250 10b 4:4:4 | 0/0/0 | 0/0/0 |
| `s444_352x242_10b` | 352x242 10b 4:4:4 | 0/0/0 | 0/0/0 |
| `s444_320x250_10b` | 320x250 10b 4:4:4 | 0/0/0 | 0/0/0 |

25 of 26 byte-exact after the change; six cells moved; **no 4:2:0 and no
4:4:4 cell moved**, which is the load-bearing control (the delta is
4:2:2-specific by construction and by measurement).

### 5.2 Per-frame, per-plane, every cell the change touched

See §6 of this file for the full per-frame tables (16 rows per cell).

### 5.3 DECODE order (17 frames, hidden alt-ref included) vs instrumented aomdec

| cell | pre-fix | post-fix |
|---|---|---|
| `s422_416x250_10b` | DIVERGES (the census's 17-frame 103250/64191/62834) | **17/17 BYTE-EXACT**, 0 wrong samples, no frame with any |
| `s422_416x242_10b` | DIVERGES | **17/17 BYTE-EXACT** |
| `s422_320x246` | DIVERGES | **17/17 BYTE-EXACT** |

The hidden alt-ref frame is in that count: `ls` shows 17 `.f<N>` dumps per
side, and the comparator hard-errors on a frame-count mismatch, so the
16-shown/17-decode distinction is measured, not assumed.

## 6. Per-frame tables

### `s422_416x250_10b` (416x250 ss=10 depth=10), 16 shown frames, wrong samples Y/U/V
| frame | pre Y | pre U | pre V | post Y | post U | post V |
|---|---|---|---|---|---|---|
| 0 **<-first** | 6538 | 3527 | 3514 | 0 | 0 | 0 |
| 1 | 7159 | 4303 | 4259 | 0 | 0 | 0 |
| 2 | 7135 | 4341 | 4268 | 0 | 0 | 0 |
| 3 | 6729 | 3980 | 3992 | 0 | 0 | 0 |
| 4 | 6560 | 4118 | 3969 | 0 | 0 | 0 |
| 5 | 6549 | 3985 | 3869 | 0 | 0 | 0 |
| 6 | 6952 | 4152 | 4105 | 0 | 0 | 0 |
| 7 | 6335 | 3752 | 3698 | 0 | 0 | 0 |
| 8 | 6239 | 3990 | 3855 | 0 | 0 | 0 |
| 9 | 6272 | 4113 | 3939 | 0 | 0 | 0 |
| 10 | 5999 | 4089 | 3922 | 0 | 0 | 0 |
| 11 | 5760 | 3596 | 3488 | 0 | 0 | 0 |
| 12 | 5814 | 3810 | 3598 | 0 | 0 | 0 |
| 13 | 5485 | 3548 | 3395 | 0 | 0 | 0 |
| 14 | 5246 | 3477 | 3382 | 0 | 0 | 0 |
| 15 | 4239 | 2705 | 2853 | 0 | 0 | 0 |
| **total** | **99011** | **61486** | **60106** | **0** | **0** | **0** |

### `s422_416x242_10b` (416x242 ss=10 depth=10), 16 shown frames, wrong samples Y/U/V
| frame | pre Y | pre U | pre V | post Y | post U | post V |
|---|---|---|---|---|---|---|
| 0 **<-first** | 0 | 63 | 59 | 0 | 0 | 0 |
| 1 | 0 | 380 | 317 | 0 | 0 | 0 |
| 2 | 0 | 434 | 348 | 0 | 0 | 0 |
| 3 | 0 | 288 | 218 | 0 | 0 | 0 |
| 4 | 0 | 332 | 258 | 0 | 0 | 0 |
| 5 | 0 | 356 | 293 | 0 | 0 | 0 |
| 6 | 0 | 451 | 339 | 0 | 0 | 0 |
| 7 | 0 | 292 | 229 | 0 | 0 | 0 |
| 8 | 0 | 419 | 281 | 0 | 0 | 0 |
| 9 | 0 | 437 | 312 | 0 | 0 | 0 |
| 10 | 0 | 541 | 384 | 0 | 0 | 0 |
| 11 | 0 | 338 | 266 | 0 | 0 | 0 |
| 12 | 0 | 263 | 218 | 0 | 0 | 0 |
| 13 | 0 | 408 | 290 | 0 | 0 | 0 |
| 14 | 0 | 506 | 358 | 0 | 0 | 0 |
| 15 | 0 | 232 | 178 | 0 | 0 | 0 |
| **total** | **0** | **5740** | **4348** | **0** | **0** | **0** |

### `s422_352x242_10b` (352x242 ss=10 depth=10), 16 shown frames, wrong samples Y/U/V
| frame | pre Y | pre U | pre V | post Y | post U | post V |
|---|---|---|---|---|---|---|
| 0 **<-first** | 0 | 85 | 84 | 0 | 0 | 0 |
| 1 | 0 | 194 | 169 | 0 | 0 | 0 |
| 2 | 0 | 232 | 197 | 0 | 0 | 0 |
| 3 | 0 | 245 | 199 | 0 | 0 | 0 |
| 4 | 0 | 268 | 201 | 0 | 0 | 0 |
| 5 | 0 | 280 | 223 | 0 | 0 | 0 |
| 6 | 0 | 349 | 279 | 0 | 0 | 0 |
| 7 | 0 | 241 | 202 | 0 | 0 | 0 |
| 8 | 0 | 324 | 233 | 0 | 0 | 0 |
| 9 | 0 | 406 | 320 | 0 | 0 | 0 |
| 10 | 0 | 470 | 371 | 0 | 0 | 0 |
| 11 | 0 | 368 | 302 | 0 | 0 | 0 |
| 12 | 0 | 390 | 323 | 0 | 0 | 0 |
| 13 | 0 | 301 | 244 | 0 | 0 | 0 |
| 14 | 0 | 295 | 258 | 0 | 0 | 0 |
| 15 | 0 | 205 | 172 | 0 | 0 | 0 |
| **total** | **0** | **4653** | **3777** | **0** | **0** | **0** |

### `s422_320x246` (320x246 ss=10 depth=8), 16 shown frames, wrong samples Y/U/V
| frame | pre Y | pre U | pre V | post Y | post U | post V |
|---|---|---|---|---|---|---|
| 0 **<-first** | 0 | 58 | 51 | 0 | 0 | 0 |
| 1 | 0 | 269 | 169 | 0 | 0 | 0 |
| 2 | 0 | 308 | 225 | 0 | 0 | 0 |
| 3 | 0 | 371 | 278 | 0 | 0 | 0 |
| 4 | 0 | 410 | 310 | 0 | 0 | 0 |
| 5 | 0 | 348 | 254 | 0 | 0 | 0 |
| 6 | 0 | 580 | 402 | 0 | 0 | 0 |
| 7 | 0 | 608 | 425 | 0 | 0 | 0 |
| 8 | 0 | 620 | 423 | 0 | 0 | 0 |
| 9 | 0 | 634 | 430 | 0 | 0 | 0 |
| 10 | 0 | 621 | 444 | 0 | 0 | 0 |
| 11 | 0 | 655 | 481 | 0 | 0 | 0 |
| 12 | 0 | 640 | 460 | 0 | 0 | 0 |
| 13 | 0 | 575 | 405 | 0 | 0 | 0 |
| 14 | 0 | 584 | 400 | 0 | 0 | 0 |
| 15 | 0 | 529 | 374 | 0 | 0 | 0 |
| **total** | **0** | **7810** | **5531** | **0** | **0** | **0** |

### `s422_322x240` (322x240 ss=10 depth=8), 16 shown frames, wrong samples Y/U/V
| frame | pre Y | pre U | pre V | post Y | post U | post V |
|---|---|---|---|---|---|---|
| 0 **<-first** | 0 | 1786 | 1784 | 0 | 0 | 0 |
| 1 | 0 | 1786 | 1766 | 0 | 0 | 0 |
| 2 | 0 | 1871 | 1815 | 0 | 0 | 0 |
| 3 | 0 | 1701 | 1718 | 0 | 0 | 0 |
| 4 | 0 | 1642 | 1641 | 0 | 0 | 0 |
| 5 | 0 | 1577 | 1611 | 0 | 0 | 0 |
| 6 | 0 | 1550 | 1508 | 0 | 0 | 0 |
| 7 | 0 | 1440 | 1459 | 0 | 0 | 0 |
| 8 | 0 | 1180 | 1120 | 0 | 0 | 0 |
| 9 | 0 | 1204 | 1170 | 0 | 0 | 0 |
| 10 | 0 | 1240 | 1184 | 0 | 0 | 0 |
| 11 | 0 | 1125 | 1096 | 0 | 0 | 0 |
| 12 | 0 | 1035 | 1040 | 0 | 0 | 0 |
| 13 | 0 | 922 | 912 | 0 | 0 | 0 |
| 14 | 0 | 912 | 891 | 0 | 0 | 0 |
| 15 | 0 | 1032 | 910 | 0 | 0 | 0 |
| **total** | **0** | **22003** | **21625** | **0** | **0** | **0** |

### `s422_322x246` (322x246 ss=10 depth=8), 16 shown frames, wrong samples Y/U/V
| frame | pre Y | pre U | pre V | post Y | post U | post V |
|---|---|---|---|---|---|---|
| 0 **<-first** | 0 | 62 | 47 | 0 | 0 | 0 |
| 1 | 0 | 233 | 145 | 0 | 0 | 0 |
| 2 | 0 | 267 | 174 | 0 | 0 | 0 |
| 3 | 0 | 212 | 142 | 0 | 0 | 0 |
| 4 | 0 | 238 | 150 | 0 | 0 | 0 |
| 5 | 0 | 332 | 212 | 0 | 0 | 0 |
| 6 | 0 | 255 | 167 | 0 | 0 | 0 |
| 7 | 0 | 118 | 87 | 0 | 0 | 0 |
| 8 | 0 | 184 | 113 | 0 | 0 | 0 |
| 9 | 0 | 316 | 219 | 0 | 0 | 0 |
| 10 | 0 | 204 | 131 | 0 | 0 | 0 |
| 11 | 0 | 190 | 124 | 0 | 0 | 0 |
| 12 | 0 | 244 | 177 | 0 | 0 | 0 |
| 13 | 0 | 167 | 93 | 0 | 0 | 0 |
| 14 | 0 | 249 | 139 | 0 | 0 | 0 |
| 15 | 0 | 166 | 102 | 0 | 0 | 0 |
| **total** | **0** | **3437** | **2222** | **0** | **0** | **0** |

### `s422_384x240` (384x240 ss=10 depth=8), 16 shown frames, wrong samples Y/U/V
| frame | pre Y | pre U | pre V | post Y | post U | post V |
|---|---|---|---|---|---|---|
| 0 **<-first** | 0 | 0 | 0 | 0 | 0 | 0 |
| 1 | 0 | 0 | 0 | 0 | 0 | 0 |
| 2 | 0 | 0 | 0 | 0 | 0 | 0 |
| 3 | 0 | 0 | 0 | 0 | 0 | 0 |
| 4 | 0 | 0 | 0 | 0 | 0 | 0 |
| 5 | 0 | 0 | 0 | 0 | 0 | 0 |
| 6 | 0 | 4 | 12 | 0 | 4 | 12 |
| 7 | 0 | 50 | 60 | 0 | 50 | 60 |
| 8 | 0 | 96 | 104 | 0 | 96 | 104 |
| 9 | 0 | 138 | 150 | 0 | 138 | 150 |
| 10 | 0 | 210 | 210 | 0 | 210 | 210 |
| 11 | 0 | 250 | 258 | 0 | 250 | 258 |
| 12 | 0 | 292 | 303 | 0 | 292 | 303 |
| 13 | 0 | 290 | 303 | 0 | 290 | 303 |
| 14 | 0 | 248 | 261 | 0 | 248 | 261 |
| 15 | 0 | 202 | 210 | 0 | 202 | 210 |
| **total** | **0** | **1780** | **1871** | **0** | **1780** | **1871** |

### `s420_416x250_10b` (416x250 ss=11 depth=10), 16 shown frames, wrong samples Y/U/V
| frame | pre Y | pre U | pre V | post Y | post U | post V |
|---|---|---|---|---|---|---|
| 0 **<-first** | 0 | 0 | 0 | 0 | 0 | 0 |
| 1 | 0 | 0 | 0 | 0 | 0 | 0 |
| 2 | 0 | 0 | 0 | 0 | 0 | 0 |
| 3 | 0 | 0 | 0 | 0 | 0 | 0 |
| 4 | 0 | 0 | 0 | 0 | 0 | 0 |
| 5 | 0 | 0 | 0 | 0 | 0 | 0 |
| 6 | 0 | 0 | 0 | 0 | 0 | 0 |
| 7 | 0 | 0 | 0 | 0 | 0 | 0 |
| 8 | 0 | 0 | 0 | 0 | 0 | 0 |
| 9 | 0 | 0 | 0 | 0 | 0 | 0 |
| 10 | 0 | 0 | 0 | 0 | 0 | 0 |
| 11 | 0 | 0 | 0 | 0 | 0 | 0 |
| 12 | 0 | 0 | 0 | 0 | 0 | 0 |
| 13 | 0 | 0 | 0 | 0 | 0 | 0 |
| 14 | 0 | 0 | 0 | 0 | 0 | 0 |
| 15 | 0 | 0 | 0 | 0 | 0 | 0 |
| **total** | **0** | **0** | **0** | **0** | **0** | **0** |

### `s444_416x250_10b` (416x250 ss=00 depth=10), 16 shown frames, wrong samples Y/U/V
| frame | pre Y | pre U | pre V | post Y | post U | post V |
|---|---|---|---|---|---|---|
| 0 **<-first** | 0 | 0 | 0 | 0 | 0 | 0 |
| 1 | 0 | 0 | 0 | 0 | 0 | 0 |
| 2 | 0 | 0 | 0 | 0 | 0 | 0 |
| 3 | 0 | 0 | 0 | 0 | 0 | 0 |
| 4 | 0 | 0 | 0 | 0 | 0 | 0 |
| 5 | 0 | 0 | 0 | 0 | 0 | 0 |
| 6 | 0 | 0 | 0 | 0 | 0 | 0 |
| 7 | 0 | 0 | 0 | 0 | 0 | 0 |
| 8 | 0 | 0 | 0 | 0 | 0 | 0 |
| 9 | 0 | 0 | 0 | 0 | 0 | 0 |
| 10 | 0 | 0 | 0 | 0 | 0 | 0 |
| 11 | 0 | 0 | 0 | 0 | 0 | 0 |
| 12 | 0 | 0 | 0 | 0 | 0 | 0 |
| 13 | 0 | 0 | 0 | 0 | 0 | 0 |
| 14 | 0 | 0 | 0 | 0 | 0 | 0 |
| 15 | 0 | 0 | 0 | 0 | 0 | 0 |
| **total** | **0** | **0** | **0** | **0** | **0** | **0** |

### `s420_320x242_10b` (320x242 ss=11 depth=10), 16 shown frames, wrong samples Y/U/V
| frame | pre Y | pre U | pre V | post Y | post U | post V |
|---|---|---|---|---|---|---|
| 0 **<-first** | 0 | 0 | 0 | 0 | 0 | 0 |
| 1 | 0 | 0 | 0 | 0 | 0 | 0 |
| 2 | 0 | 0 | 0 | 0 | 0 | 0 |
| 3 | 0 | 0 | 0 | 0 | 0 | 0 |
| 4 | 0 | 0 | 0 | 0 | 0 | 0 |
| 5 | 0 | 0 | 0 | 0 | 0 | 0 |
| 6 | 0 | 0 | 0 | 0 | 0 | 0 |
| 7 | 0 | 0 | 0 | 0 | 0 | 0 |
| 8 | 0 | 0 | 0 | 0 | 0 | 0 |
| 9 | 0 | 0 | 0 | 0 | 0 | 0 |
| 10 | 0 | 0 | 0 | 0 | 0 | 0 |
| 11 | 0 | 0 | 0 | 0 | 0 | 0 |
| 12 | 0 | 0 | 0 | 0 | 0 | 0 |
| 13 | 0 | 0 | 0 | 0 | 0 | 0 |
| 14 | 0 | 0 | 0 | 0 | 0 | 0 |
| 15 | 0 | 0 | 0 | 0 | 0 | 0 |
| **total** | **0** | **0** | **0** | **0** | **0** | **0** |

### `s444_352x242_10b` (352x242 ss=00 depth=10), 16 shown frames, wrong samples Y/U/V
| frame | pre Y | pre U | pre V | post Y | post U | post V |
|---|---|---|---|---|---|---|
| 0 **<-first** | 0 | 0 | 0 | 0 | 0 | 0 |
| 1 | 0 | 0 | 0 | 0 | 0 | 0 |
| 2 | 0 | 0 | 0 | 0 | 0 | 0 |
| 3 | 0 | 0 | 0 | 0 | 0 | 0 |
| 4 | 0 | 0 | 0 | 0 | 0 | 0 |
| 5 | 0 | 0 | 0 | 0 | 0 | 0 |
| 6 | 0 | 0 | 0 | 0 | 0 | 0 |
| 7 | 0 | 0 | 0 | 0 | 0 | 0 |
| 8 | 0 | 0 | 0 | 0 | 0 | 0 |
| 9 | 0 | 0 | 0 | 0 | 0 | 0 |
| 10 | 0 | 0 | 0 | 0 | 0 | 0 |
| 11 | 0 | 0 | 0 | 0 | 0 | 0 |
| 12 | 0 | 0 | 0 | 0 | 0 | 0 |
| 13 | 0 | 0 | 0 | 0 | 0 | 0 |
| 14 | 0 | 0 | 0 | 0 | 0 | 0 |
| 15 | 0 | 0 | 0 | 0 | 0 | 0 |
| **total** | **0** | **0** | **0** | **0** | **0** | **0** |

## 7. What this lane did not do, and what is left

* **`s422_384x240` (384x240 8-bit) is unchanged at 0/1780/1871** and is NOT
  this arm: it is luma-exact both before and after, and its wrong region is
  the census's "starts at decode frame 1, 11/17 frames" cell. Not
  investigated here; named so it is not mistaken for a closed cell.
* The 4:4:4 intra-BC chroma residual the 4:4:4 gate already names (first
  wrong sample index 20532, entropy fork read 12465 at mi (90,108)) is
  untouched, as its numbers show.
* No product decision is recommended: the 4:2:2 sequence-header refusal is
  still in the tree, and lifting it is a separate step.
* The bypass, the `EC_TU`/`EC_RCT` probe rungs and the sweep scripts live
  in `/home/tahinli/.cache/w422luma/` and the detached probe worktree. None
  of them is in the commit; `git status --porcelain` on this branch is
  empty apart from this report.
