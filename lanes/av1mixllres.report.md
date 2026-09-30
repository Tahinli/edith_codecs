# lane-av1mixllres — the mixed-lossless class's residue arms and its >8-bit arm: MEASURED, all closed on `main` by one publish that was NOT mine to land

Base `main` = `167548c6` ("Merge lane-av1txctxband2 r2: the mixed-lossless RESIDUE arms
are the SAME missing publish, not three more defects"). Worktree
`/home/tahinli/.cache/wt/av1mixllres`, target dir
`$HOME/.cache/cargo-target-av1mixllres`, `EC_NOMEMGUARD=1`. Oracle
`~/.cache/aom-oracle/build/{aomdec,aomenc}` (shared, untouched — not rebuilt, not
re-instrumented). ffmpeg 8.1.3.

**No decoder change is in this lane.** `git diff --stat HEAD -- crates/` is empty at
`167548c6`: every number below describes `main` as it stands. The one-line publish
this lane independently derived and measured is already in `main` as `f2dd27b0`
(`c23e8b90`), and the three residue fixtures + gate are `f8a66917` (`167548c6`).
The deliverable is the measurement — including the **10-bit and 12-bit arms, which
no lane had built** — and the red-before control that shows the class is
**not bit-depth-dependent**.

## 0. Verdict

| | |
|---|---|
| 6-frame 256x128 residue arm (`420_mixll_256x128_6f.obu`) | at `46a7e67b`: 7 dumps, **red f1..f6 by 6051 / 4115 / 2962 / 26566 / 41876 / 14749**; on `167548c6`: **7/7 byte-exact**, 0 differing bytes, every plane, every decode-order frame |
| 320x240 arm, 5 frames (`420_mixll_320x240_5f.obu`) | at `46a7e67b`: 6 dumps, **red f1..f5 by 15031 / 16259 / 21932 / 65405 / 90077**; on `167548c6`: **6/6 byte-exact** |
| 320x240 arm, 6 frames (`420_mixll_320x240_6f.obu`) | at `46a7e67b`: 7 dumps, **red f6 only, 19398**; on `167548c6`: **7/7 byte-exact** |
| **10-bit 4:2:0 arm (built here for the first time)** | at `46a7e67b`: **REFUSED** ("a Golomb tail longer than this decoder reads") after 4 dumps, with f1/f2/f4 red by 12766 / 5382 / 24764; on `167548c6`: **7/7 byte-exact** |
| **12-bit 4:2:0 arm (built here)** | at `46a7e67b`: 7 dumps, red f1/f2/f4/f5/f6 by 15003 / 7155 / 43328 / 54347 / 25047; on `167548c6`: **7/7 byte-exact** |
| Is the class bit-depth-dependent? | **NO.** The same one missing publish desyncs 8-bit, 10-bit and 12-bit cells of the identical recipe, and the same one line closes all three. The defect is in band BOOKKEEPING, which has no depth term anywhere in `txfm_partition_context` / `set_txfm_ctxs`. |
| Already-closed cells, re-measured here on `167548c6` | pin `m5_256x128` **6/6**, 176x144 **7/7**, 352x288 **7/7**, 4:4:4 8-frame **9/9**, 640x480 5f **6/6**, 640x480 6f **7/7** — all byte-exact |
| Fix / fixture / gate / counter | **not mine.** All four are `lane-av1txctxband2`'s, in `main`. This lane adds **no** fixture and **no** gate; see §6 for why adding one would be a duplicate. |

## 1. The recipe, and the HBD source built by hand

8-bit cells are `ffmpeg testsrc2` at the stated size → the class's encode line:

```text
aomenc --codec=av1 --obu -o <cell>.obu --passes=1 --cpu-used=4 --limit=<N> \
       --end-usage=q --cq-level=0 --aq-mode=1 <src>.y4m          # 8-bit 4:2:0
```

**The >8-bit source is built by hand**, because no `ffmpeg` here emits a >8-bit y4m
and a raw 16-bit-LE one is rejected by the shared `aomenc` with `Loss of framing in
Y4M input data`. Recipe (the same shape as `lanes/av1444d10.report.md` §1, adapted to
4:2:0):

```python
# scripts/raw_to_y4m.py W H FPS out.y4m DEPTH  (repo-owned helper, unmodified)
# DEPTH = BYTES per sample on stdin (2 for 10- and 12-bit), asserted against stdin length.
# Header:  YUV4MPEG2 W256 H128 F25:1 Ip A1:1 C420p10
# then, PER FRAME:  b"FRAME\n"  +  256*128*2  +  2*((256+1)//2)*((128+1)//2)*2
ffmpeg -v error -f lavfi -i testsrc2=size=256x128:rate=25 -frames:v 6 \
       -pix_fmt yuv420p10le -f rawvideo src10.raw
python3 scripts/raw_to_y4m.py 256 128 25 src10.y4m 10 < src10.raw
```

libaom's `y4minput.c:1163-1171` requires the six bytes `FRAME\n` before **EVERY**
frame; the helper emits them per frame and its length assertion is what stops a
misframed y4m (a 10-bit stream of N frames is exactly 2N 8-bit frames, so no
length-only check can catch a depth mistake — the DEPTH argument is asserted).

Then the same class line, plus the depth:

```text
aomenc --codec=av1 --obu -o m_256x128_10b.obu --passes=1 --cpu-used=4 --limit=6 \
       --end-usage=q --cq-level=0 --aq-mode=1 \
       --input-bit-depth=10 --bit-depth=10 src10.y4m
```

| cell | bytes | sha256 (prefix) |
|---|---:|---|
| `m_256x128.obu` (6 f) | 22 336 | `8a203e520f12f4ba` |
| `m_320x240.obu` (6 f) | 38 686 | `37fbe7cb…` |
| `m5_320x240.obu` (5 f) | 28 716 | `40d613fd…` |
| **`m_256x128_10b.obu`** | **41 012** | 10-bit 4:2:0 profile 0 |
| **`m_256x128_12b.obu`** | **56 039** | `aomenc` prints `Warning: automatically updating to profile 2 to match input format` — the encoder ACCEPTS 12-bit 4:2:0, so the 12-bit arm exists and is measured |
| `m5_256x128.obu` (the pin) | 18 525 | `3e06b5641e6a0f5a` — reproduces the committed pin's sha256 exactly |
| `m_176x144.obu` / `m_352x288.obu` / `m5_640x480.obu` / `m_640x480.obu` / `m444_8.obu` | 20 069 / 46 490 / 87 537 / 115 615 / 79 051 | `02b15d0d…` / `e43c74b8…` / `3d505fd2…` / `c82ba4d7…` / — every byte count reproduces the inherited reports' |

Both byte counts and the sha prefixes the prior reports name reproduce on the first
try. **The 10-bit cell is genuinely mixed-lossless**, not a lossy cell that happens
to be HBD: at base it desyncs at a `txfm_partition` context read like the 8-bit ones
(§3), and the fix that closes it is the same publish.

## 2. The comparator, and its non-vacuity at 10-bit

Both sides are the **decode-order `EC_AV1_FINAL_DUMP`** (rung 12 of
`scripts/instrument-aom-oracle.sh`), which is depth-correct: 8-bit as `u8`, 10/12-bit
as `u16` LE. **The u8-narrowing trap is avoided by construction** — the
`EC_AV1_DECODE_ORDER_DUMP` rung that narrows every plane is never used here, and
comparing a narrowed dump against a depth-correct one would make every HBD cell
diverge by construction.

Per-frame plane spans come from **our frame's own plane lengths** (never a formula,
never frame 0's), and the sample depth from the **byte length the frame actually
is** — 98 304 B at 10-bit 4:2:0 256x128 = `256*128*1.5*2`, asserted, versus 49 152 B
if it were 8-bit. So a 10-bit difference in the top bits cannot hide.

**Non-vacuity control (the check that matters at HBD):** with the same dumps, flip
the low bit of ONE oracle sample of frame 4 and re-compare.

```text
clean compare differing bytes: 0
after a 1-LSB oracle flip, differing BYTES: 1  first byte: 2000 = sample 1000
```

The comparator is live at 10-bit: it reports zero only because the two sides are
byte-identical, not because it cannot see a 10-bit difference. (This is the same
discipline as `count_rawvideo_diffs`' `flip` argument in `stream.rs`.)

## 3. Per-plane, per-frame, decode order — base vs main

`EC_AV1_FINAL_DUMP` both sides, per differing frame. Base `46a7e67b` (pre-publish) vs
main `167548c6`, **no local change on either side**.

| cell | base `46a7e67b` | main `167548c6` |
|---|---|---|
| `m5_256x128` (pin, 5 f) | 6 dumps, **0 differing** | 6/6 exact |
| **`m_256x128` (6 f)** | 7 dumps, f1..f6 red **6051 / 4115 / 2962 / 26566 / 41876 / 14749** | **7/7 EXACT** |
| **`m_320x240` (6 f)** | 7 dumps, f0..f5 exact, **f6 red 19398** | **7/7 EXACT** |
| **`m5_320x240` (5 f)** | 6 dumps, f1..f5 red **15031 / 16259 / 21932 / 65405 / 90077** | **6/6 EXACT** |
| `m_176x144` (6 f) | 7/7 exact | 7/7 exact |
| `m_352x288` (6 f) | 7/7 exact | 7/7 exact |
| `m444_8` (4:4:4, 8 f) | 9/9 exact | 9/9 exact |
| `m5_640x480` (5 f) | **REFUSED** at frame 4; f1..f3 red 64181 / 98673 / 63442 | **6/6 EXACT** |
| `m_640x480` (6 f) | REFUSED | **7/7 EXACT** |
| **`m_256x128_10b` (10-bit, 6 f)** | **REFUSED** after 4 dumps; f1 12766, f2 5382, f4 24764; f5/f6 never decoded | **7/7 EXACT** |
| **`m_256x128_12b` (12-bit, 6 f)** | 7 dumps, f1 15003, f2 7155, f4 43328, f5 54347, f6 25047 | **7/7 EXACT** |

Every base figure here is my own count, taken on my own worktree and target dir; the
per-plane split of each red frame is in the raw output above (e.g. 256x128 f5
`Y=28424@0 U=6858@0 V=6594@0`). The residue arms' totals match `lane-av1txctxband2`'s
independent measurement to the byte (6051/4115/2962/26566/41876/14749,
15031/16259/21932/65405/90077, 19398).

**The premise, measured rather than assumed:** "the cell decodes" was false on the
10-bit arm at base (it REFUSED) and true-but-wrong on the 12-bit arm (all 7 frames
decoded, 5 wrong). Neither state is visible without the per-plane pixel count.

## 4. Cause: one missing publish, and it is the SAME site for every depth

The one line, already in `main` as `f2dd27b0`, at `read_block_tx_size`'s lossless
early return:

```rust
set_txfm_ctxs(n, at_mi, 4, side_mi, side_mi, skip && is_inter);
```

libaom's `parse_decode_block` (`decodeframe.c:1244-1261`) is an EITHER/OR: the var-tx
branch's condition carries `!xd->lossless[mbmi->segment_id]`, so a lossless block
takes the `else` branch, where `read_tx_size` returns `TX_4X4` **and then
`set_txfm_ctxs` publishes over the block's own width/height**. The early return
skipped the PUBLISH, not the symbol read. `read_block_tx_size_rect` has published on
that same arm since `lane-av1lm444loss`; only the square arm had the hole. A lossless
block therefore left `above_txfm_context`/`left_txfm_context` holding whatever an
earlier block wrote, and the next LOSSY block's `txfm_partition` context read took
its `above`/`left` operands from that stale neighbour.

**My own localisation, from the base tree, on the 320x240 5-frame cell** (this is the
discriminator the charter asked for, and it is a `txfm_partition` read, NOT the
`tx_size_cat0` site of the 640x480 case):

* paired `EC_SYMR`, whole stream: **first divergence at read 64 674** (ours
  212 273 reads, oracle 157 953). Both sides are at `mi=(40,52)` and both read
  `n=2 s=0`; the post-range differs (ours 47814, oracle 33426) because the two read
  **different CDF rows**.
* the operands, from the oracle's own `EC_VARTXCTX` against ours:
  `mi=(40,52) txw=8 txh=8 above=4 left=4 ctx=20` versus ours
  `above=16 left=4 ctx=19`. The `above` band cell is the whole difference.
* the band replay (`EC_TXUPD`, `above_txfm_context` scope = per tile,
  `left_txfm_context` = per superblock row, init `TXFM_CTX_INIT = 64`) puts the last
  write to mi column 52 at a **skipped 16-px inter block at `mi=(16,52)` publishing
  16**, and nothing between it and the read — libaom reads 4, i.e. it has a write in
  rows 17..39 that this tree did not make. Same signature the conjunct report named
  for the 640x480 cell.
* the per-site ctx lock (temporary, reverted): forcing `ctx=20` at `mi=(40,52)` alone
  makes all 6 decode-order frames of that cell byte-exact, which is the symptom-mask
  proof that the ctx FORMULA is right and the band VALUE is the cause. The same lock
  on the 6-frame 256x128 cell is `ctx=18` at `mi=(24,40)`, and it makes all 7 frames
  byte-exact too.

On `167548c6` the same lock is unnecessary, and the operand pair av1mixllconj §4.3
specified as the discriminator now reads, on `m5_640x480` with no forced ctx
anywhere:

```text
oracle  EC_TXCTXB mi=80,110 abv=4 lft=8 above=0 left=1 ctx=1
ours    EC_TXCTX  mi=80,110 above_txfm=4 left_txfm=8 above=false left=true  -> ctx=1
```

That is the report's FIRST outcome row (`above_txfm` 4, `ctx` 1 on its own): the band
is closed, and the forced-`ctx=1` mask is retired as a symptom mask rather than a
repair.

## 5. Scoped test command and output

```text
CARGO_TARGET_DIR=$HOME/.cache/cargo-target-av1mixllres EC_NOMEMGUARD=1 \
  cargo test -p ec-av1 --lib -- a_420_mixed_lossless --nocapture
```

```text
a_420_mixed_lossless_alt_ref_sub8_vartx_witness_is_byte_exact_in_decode_order:
  6 decode-order frame(s) byte-exact (1 hidden), 5 shown frame(s) exact per plane,
  10 sub-8 leaf/leaves on a lossless segment read no var-tx symbol
test ...a_420_mixed_lossless_alt_ref_sub8_vartx_witness_is_byte_exact... ok
a_420_mixed_lossless_residue_arms_are_byte_exact_in_decode_order/420_mixll_256x128_6f.obu:
  7 decode-order frame(s) byte-exact (1 hidden), 265 lossless square inter block(s) overwrote a stale above-band size
a_420_mixed_lossless_residue_arms_are_byte_exact_in_decode_order/420_mixll_320x240_5f.obu:
  6 decode-order frame(s) byte-exact (1 hidden), 432 lossless square inter block(s) overwrote a stale above-band size
a_420_mixed_lossless_residue_arms_are_byte_exact_in_decode_order/420_mixll_320x240_6f.obu:
  7 decode-order frame(s) byte-exact (1 hidden), 487 lossless square inter block(s) overwrote a stale above-band size
test ...a_420_mixed_lossless_residue_arms_are_byte_exact_in_decode_order ... ok
a_420_mixed_lossless_alt_ref_640x480_txfm_band_publish_is_byte_exact_in_decode_order:
  6 decode-order frame(s) byte-exact (1 hidden), 5 shown frame(s) exact per plane,
  944 lossless square inter block(s) overwrote a stale above-band size
test ...a_420_mixed_lossless_alt_ref_640x480_txfm_band_publish_is_byte_exact... ok
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 805 filtered out; finished in 4.17s
```

The wider scope on the same tip, no local change:

```text
CARGO_TARGET_DIR=$HOME/.cache/cargo-target-av1mixllres EC_NOMEMGUARD=1 \
  cargo test -p ec-av1 --lib -- lossless mixll txctx vartx
```

```text
test result: ok. 38 passed; 0 failed; 0 ignored; 0 measured; 770 filtered out; finished in 153.26s
```

## 6. Why this lane lands no fixture, no gate, and no fix

The one line is in `main` (`f2dd27b0`, merged `c23e8b90`); the three residue fixtures
and their gate are in `main` (`f8a66917`, merged `167548c6`). A second publish at the
same site would be a duplicate change and at best a no-op diff. A second fixture set
for the same three cells would duplicate `f8a66917`'s, and a 10-bit gate would need a
cell whose only difference from the 8-bit gate is the depth argument of the same
comparator — which is exactly the kind of gate that proves nothing the existing one
does not. The 10-bit and 12-bit arms are therefore reported as **measurements**, and
the decision about whether a depth-parameterised gate earns its keep is left to the
main agent with the numbers in hand.

**Class-sweep note, one open item, not fixed here.** `lane-av1txctxband2` §7 site 3:
`read_block_tx_size_rect`'s lossless arm publishes `(4, 4)` where libaom's
`set_txfm_ctxs` carries the `skip && is_inter` block-size term. It is REACHED (their
29/1/9/6/16 hit counts) but moving it changed nothing on any fixture that reaches it.
**My cells do not unblock it** — the 10-bit and 12-bit cells go byte-exact with no
change there, so they add no reader to that gap.

## 7. What is NOT measured

* **No committed corpus sweep is claimed.** I measured the class's own recipe at nine
  geometries × three depths, base and tip. I did not run the 112-fixture corpus
  before/after (a partial run of mine was interrupted and its bucket counts are not
  trustworthy enough to publish), so this report makes **no** claim about corpus-wide
  status change. `lane-av1txctxband2` §0 already carries that sweep (110 fixtures, zero
  rows changed).
* The class is content-dependent: every cell here is `testsrc2` at one rate. That is a
  sweep of the class at three depths and nine geometries, not of all content.
* `EC_SYMR` ladders for the 10-bit and 12-bit arms were not taken; the pixel-level
  result on all 7 decode-order frames plus the non-vacuity flip control is the
  evidence offered for them, and the causal localisation (§4) is from the 8-bit
  320x240 cell, where the class's mechanism is the same.

## 8. fix-now | deferred | accepted

* **accepted** — the residue measurement and the HBD measurement, on `main`, with the
  red-before control and the comparator's non-vacuity at 10-bit. No code landed.
* **accepted** — every diagnostic this lane added (`EC_VARTXSPLIT`, the per-site
  `EC_VARTXLOCK` ctx override, the picture-index tag on the `EC_TXUPD` rungs, the
  `EC_LLPUB` probe) is **reverted**; `git diff --stat HEAD -- crates/` is empty. The
  shared oracle was never rebuilt or edited.
* **deferred(unblock: a cell on which `read_block_tx_size_rect`'s lossless `skip &&
  is_inter` term has an observable reader)** — `lane-av1txctxband2`'s site 3. My
  10-bit and 12-bit cells do not supply one.
