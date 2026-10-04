# lane-av1422ibcrect — the non-split chroma gather of `decode_intrabc_owned_rect` is
# WITNESSED at ss (1, 0): a 4:2:2 64x32 intraBC strip enters it, 24 times, byte-exact

Base `main` = `5824d7ce` (branch `lane/av1422ibcrect`, worktree
`~/.cache/wt/av1422ibcrect`). Local only, no push, no merge, no source change.

`lanes/av1422gathercensus.report.md` closed the ss (1, 0) chroma-gather
enumeration with one row left **STOP — unwitnessed**: `decode_intrabc_owned_rect`
(`decode.rs:18022/18061` at that report's main), reached 0 times by any committed
4:2:2 cell. This lane asked the complementary question — can a stream be *made*
to enter it, and what happens when it does — and answered it with four aomenc
encodes.

## Verdict

**entered, 24 entries, 0 divergences.** One of the four streams (recipe A1)
enters the non-split chroma arm of `decode_intrabc_owned_rect` at ss (1, 0)
**24 times** (6 strips x 4 frames) with `bw=64 bh=32 cw=32 ch=32 skip=false`,
and decodes **byte-exact against ffmpeg on all four frames**. No first wrong
sample exists to name on this stream, and the reason is measured, not assumed
(section 4): on every one of the 24 entries the luma-span gather and the pair
rule produce the **same `dc_sign_ctx`**.

This is neither of the ticket's two nominal verdicts (`entered, first sample
named` / `4 attempts, 0 entries`) — the site is *entered* and *inert*. Both
halves are measurements; section 5 records the second finding the entry
produced (the census's named site is the *other* branch of the same `if`, and
that branch is structurally out of reach under `TX_MODE_LARGEST`).

## 1. Entry condition, and the frame shape that reaches the non-split arm

`decode_intrabc_owned_rect` has exactly one caller — `decode_block_rect64`
(`decode.rs:20076`), itself called only from the superblock-level partition
dispatch at the 64 root (`decode.rs:38506`, `38530`, `38552`, `38576`, `38649`,
`38724`). The non-split chroma gather sits in the `else` of
`if cw.min(32) != cw || ch.min(32) != ch` — in **both** of that block's residual
branches (the `leaves.is_some()` one and its `leaves == None` twin, section 5).
The condition is

    cw = bw >> ss_x(fctx)  <= 32   AND   ch = bh >> ss_y(fctx) <= 32

with `leaves.is_some()` (non-lossless) and `skip == false`. At ss (1, 0) that is
`bw <= 64 && bh <= 32`; `decode_block_rect64`'s only such shapes are **64x32**
(`PARTITION_HORZ`, and the 64x32 strip of `HORZ_A`/`HORZ_B`) and **64x16**
(`PARTITION_HORZ_4`). A 32x64 / 16x64 strip gives `ch = 64 > 32` and takes the
split arm; a 128-root strip never reaches this function at all (it goes to
`decode_block_128rect`).

So the frame shape is: **4:2:2, `--sb-size=64`, a 64x32 intraBC (screen-content,
`use_intrabc` on) HORZ strip, lossy, not all-zero residual** — the shape the
committed 4:2:2 corpus does not code. The cheapest way to force the 64x32 HORZ
at the 64 root is the frame edge: a frame whose last superblock row has only its
top 32 rows coded has `has_rows == false`, and the 64-root partition is then a
single gathered bit (HORZ or SPLIT) — no alphabet symbol (`decode.rs:36997-37045`).
Recipe A1 below is exactly that: 384x288 = 4x64 + 32.

## 2. The four recipes (aomenc count: 4, all <= 180 s)

Common flags (all four): `--codec=av1 --i422 --profile=2 --passes=1 --end-usage=q
--cpu-used=0 --threads=1 --row-mt=0 --lag-in-frames=0 --kf-max-dist=1
--tile-columns=0 --sb-size=64 --min-partition-size=32 --max-partition-size=64
--enable-rect-partitions=1 --enable-ab-partitions=1 --enable-1to4-partitions=0
--enable-palette=0 --tune-content=screen --enable-intrabc=1 --deltaq-mode=0
--enable-tx-size-search=1 --obu`.

| id | source (ffmpeg lavfi) | cq | limit | obu bytes | sha256 |
|---|---|---|---|---|---|
| **A1** | `nullsrc=size=384x288:rate=25,format=yuv420p,geq=lum='128+90*sin((X+2*N)/37)+30*sin(Y/29)':cb='128+30*sin(X/23)':cr='128+30*sin((Y+N)/19)'` -> `-pix_fmt yuv422p` | 32 | 4 | 1244 | `fd36bae423dab071c6cc964d954003a75f80c8c4cf96680181ada8c773b397db` |
| A2 | same content as A1 | 6 | 4 | 4567 | `1a165ef9d3d040a5ed29bd2cf41a16178f6cdd44dfff1b5f5596cb993118953b` |
| A3 | `geq=lum='128+90*sin((X+2*N)/37)+30*sin(Y/29)':cb='128+70*sin(X/5)':cr='128+70*sin(X/3+2*N)'` at 384x288 | 32 | 4 | 1516 | `1fcb27bb133310fb93164275a11c38a70ca3ce2a518f09deb9e21022e1c75176` |
| A4 | `testsrc2=size=128x112:rate=25` + `-vf tile=2x2` = 256x224 | 30 | 3 | 30937 | `e680b6386a8a3a961f45424dfe7a8b7faa0e43789cce38eb92f4264af20e036e` |

A1 is the 4:2:2 re-cut of `lane-av1ibc128chunk`'s `geq` sb128 intrabc recipe
(`lanes/av1ibc128chunk.report.md`), at `--sb-size=64` and a frame height that
puts the last superblock row half outside the frame. A3 changed only the chroma
frequencies (aimed at a sign-flipping above context); A4 is the ordinary
screen-content `testsrc2` tile recipe. Every encode finished in under 0.6 s.

## 3. Result per stream

Probe: a temporary env-gated `eprintln!` (`EC_IBCNS422`) at the first statement
of each non-split chroma arm, plus the pre-existing `EC_HALVSWEEP` at function
entry. Both were removed before commit (`git diff` on `decode.rs` is empty).

| id | `EC_HALVSWEEP` (function entries) | non-split chroma arm entries | decode | vs ffmpeg (yuv422p rawvideo) |
|---|---|---|---|---|
| **A1** | **24** (6/frame x 4) | **24** — all `bw=64 bh=32 cw=32 ch=32 ss=(1,0) skip=false` | 4/4 frames | **byte-exact, 4/4** (`cmp` per frame) |
| A2 | 3 | 3 | **REFUSED** mid-tile | n/a (no complete output) |
| A3 | 0 | 0 | 4/4 frames | (not compared — no entry, out of this lane's scope) |
| A4 | 0 | 0 | **REFUSED** mid-tile | n/a |

A2 and A4 both stop at the same pre-existing refusal, unrelated to this site:
`a block size 4x8, 8x16 or 16x4 ... has no chroma plane block at this frame's
subsampling mode` (the documented libaom `decodeframe.c:1456` rule). A2 entered
the site 3 times before the refusal; its output is incomplete, so it is not
offered as a pixel witness. A3's higher-frequency chroma moved the encoder off
`use_intrabc` on the edge strip entirely.

A1 detail: `SEQ: use_128x128_superblock=false ... max_frame=384x288`; entries at
`mi=(64,0) (64,16) (64,32) (64,48) (64,64) (64,80)` — mi row 64 = pixel row 256,
the half-coded last superblock row — repeated on all four frames.

## 4. Why A1 is byte-exact (measured, not inferred)

The arm computes `around = neighbours.around_mi_rect((mi_r, mi_c), bw, bh)` —
the block's **luma** span (64x32 px = 16x8 mi) — and hands `around[1]`,
`around[2]` to `read_inter_plane_rect` for planes 1 and 2, whose coefficient
context is `dc_sign_ctx(around[plane].2)` (`decode.rs:8254`: the plain
`signum` of the summed above+left dc votes). The pair rule
(`around_mi_422_chroma`, `decode.rs:10555`) samples every second above cell,
which is libaom's one-vote-per-chroma-4px-cell sum.

The probe therefore computed **both** gathers at every entry and printed the
votes, the `coded` OR-bits, and both `dc_sign_ctx` values:

| quantity over the 24 A1 entries | result |
|---|---|
| `dc_sign_ctx(full) != dc_sign_ctx(pair rule)` | **0 of 24** |
| `coded(full) != coded(pair rule)` | **0 of 24** |
| votes | mixed: `V full=-16 sampled=-8 ctx=1/1`, `V full=8 sampled=8 ctx=2/2`, `U 0/0` |

So the double count is real and visible in the magnitudes (the 16-cell sum is
twice the 8-cell sum where the extra cells are coded) but it never changes the
**sign**, and `dc_sign_ctx` is a sign — hence no symbol, no bitstream position,
no pixel differs on this stream. That is the whole reason A1 is exact.

Comparator liveness: the same print, fed a deliberately inverted vote
(`dc_sign_ctx(-vote)`), reports a difference on **24 of 24** entries — the zero
above is a measurement on this stream, not a print that cannot move. (A
narrower-span control, `around_mi_rect(bw/2)`, also read 0/24: on this stream
all above votes share one sign, so no sub-span keeps or loses a sign either.)

Scan of the cache, for context: **366** cached 4:2:2 `.obu` files
(`~/.cache/census422b`, `~/.cache/cells`, `~/.cache/lane-av1422ctintrabc`,
`~/.cache/av1muchunk422b`, `~/.cache/av1422sb128wit`, `~/.cache/av1422lrless`,
`~/.cache/av1422warp`, `~/.cache/av1422grain`) were decoded with the same probe:
**0 of them enters this site at all**. A1 is the only stream anyone has measured
in it.

## 5. Second finding: the census's cited pair is the *other* branch

The census's table row cites `decode.rs:18022/18061` at its base `2ca9e4e9`,
which on **this** base (the census's own 15-line comment sits above them) are the
pair inside `else if let Some(ls) = leaves.as_ref()` — the var-tx-tree branch:
`around_mi_rect` at **18037**, feeding `read_inter_plane_rect` plane 1 at
**18045** and plane 2 at **18062**.

The branch A1 enters is the twin in the `else` of the *same* `if cw.min(32) != cw
|| ch.min(32) != ch`, i.e. the branch taken when `read_block_tx_size_rect`
returned `None` (one transform unit, no var-tx tree): `around_mi_rect` (luma) at
**18076**, chroma gathers at **18125 (plane 1)** and **18142 (plane 2)**. Both
branches make the identical call — the whole block's luma span
`around_mi_rect((mi_r, mi_c), bw, bh)` into `read_inter_plane_rect` planes 1/2 —
so this is the same defect class. The census's §"The one remaining site" prose
describes the `leaves.is_none()` condition (the twin), while its table row cites
the tree branch's lines; the two call sites are distinct and only the no-leaves
one is entered by any stream measured here.

Measured on A1: the tree branch (its own probe line) fired **0 times**, because
`read_block_tx_size_rect` returns `Ok(None)` before it can return leaves whenever
the frame is not `TX_MODE_SELECT` (`decode.rs:31847-31855`), and A1's frames are
`txsel=false` (`TX_MODE_LARGEST`) — the probe printed `txsel=false` on all 24
entries. Under `TX_MODE_LARGEST` a <=64 block can never produce `leaves`, so:

* for a 64-root 64x32 strip, the reachable non-split chroma gather is the
  **no-leaves twin** (`18076`/`18125`/`18142`), which A1 enters;
* the tree-branch pair additionally requires `TX_MODE_SELECT` plus a
  `txfm_partition` split on the strip — a conjunction no measured stream has
  produced. (A 128-root strip *does* get `leaves` under `TX_MODE_LARGEST`, from
  the `bw.max(bh) > 64` arm at `decode.rs:31812`, which is why the r512 4:4:4
  evidence in the census is about the tree branch at another root.)

## 6. Disposition

* **Entered, inert on the only witness.** The site is no longer unwitnessed;
  it is entered at ss (1, 0) by a real aomenc 4:2:2 stream and that stream is
  byte-exact. No source change is made (the ticket forbids fixing here, and the
  one stream that enters the site gives no evidence a fix would be
  discriminated by it: 0/24 context differences, 0 coded-bit differences).
* **Not a "0 entries" verdict.** The ticket's second nominal verdict does not
  apply — A1 enters 24 times.
* **Do not re-derive from this that the class is harmless.** A1's neighbours
  happen to keep every vote's sign. The class is real where the sign flips
  (that is how the sibling sites were found); this lane only shows this *one
  64x32-edge-strip* stream does not flip it.
* **The twin call sites (`18076`/`18125`/`18142`) are the ones a future 64-root
  witness enters**; the census's cited `18022/18061` pair (this base:
  `18037`/`18045`/`18062`) needs `TX_MODE_SELECT`.
* No fixture is committed. The witness is reproducible from the A1 recipe and
  its sha256 in section 2 without adding a file the fixture-library manifest
  (`scripts/fixture-library.tsv`) does not know about.
* `decode.rs` at this lane's tip is identical to `5824d7ce` (`git diff` empty);
  the only commit is this report.

## 7. Repro

```text
git worktree add -b lane/av1422ibcrect ~/.cache/wt/av1422ibcrect 5824d7ce
cd ~/.cache/wt/av1422ibcrect
# re-add the env-gated probe at decode.rs:18126 (print `around` vs
# `neighbours.around_mi_422_chroma((mi_r, mi_c), bw, bh)` under EC_IBCNS422)
CARGO_TARGET_DIR=~/.cache/tgt-av1422ibcrect cargo build -p ec-av1 --example decode_probe

# A1 content + encode (the recipe in section 2; aomenc = ~/.cache/aom-oracle/build/aomenc)
ffmpeg -v error -f lavfi -i "nullsrc=size=384x288:rate=25,format=yuv420p,\
  geq=lum='128+90*sin((X+2*N)/37)+30*sin(Y/29)':cb='128+30*sin(X/23)':\
  cr='128+30*sin((Y+N)/19)'" -frames:v 4 -pix_fmt yuv422p -strict -1 \
  -f yuv4mpegpipe -y A1.y4m
aomenc <section 2 common flags> --cq-level=32 --limit=4 --obu -o A1.obu A1.y4m
sha256sum A1.obu   # fd36bae423dab071c6cc964d954003a75f80c8c4cf96680181ada8c773b397db

EC_IBCNS422=1 EC_HALVSWEEP=1 ~/.cache/tgt-av1422ibcrect/debug/examples/decode_probe A1.obu
#   24x EC_HALV ibc_owned ... bw=64 bh=32 skip=false
#   24x EC_IBCNS422 (no-leaves) ... ss=(1,0) skip=false txsel=false | ... ctx=N/N

# byte-exactness, display order, 4 frames
dump_yuv A1.obu ours
ffmpeg -v error -i A1.obu -pix_fmt yuv422p -f rawvideo ref.yuv
# 4 x 221184 B per frame, cmp each -> identical (evidence in section 3)
```

## 8. Evidence summary

| item | value |
|---|---|
| aomenc encodes attempted | 4 (A1 cq32 / A2 cq6 / A3 high-chroma cq32 / A4 testsrc2, all <=0.6 s) |
| streams entering the site | 1 (A1: 24 entries) |
| non-split chroma arm entries, A1 | 24 of 24 with `bw=64 bh=32 cw=32 ch=32 ss=(1,0) skip=false txsel=false` |
| tree-branch (census's `18022/18061`) entries | 0 (A1 is `TX_MODE_LARGEST`) |
| A1 vs ffmpeg | byte-exact, 4/4 frames (`cmp`, 221184 B/frame) |
| context differences (full vs pair rule), A1 | 0/24 `dc_sign_ctx`, 0/24 `coded` |
| comparator control (inverted vote) | moves 24/24 |
| cached 4:2:2 streams scanned | 366, 0 entries |
| source changes | none (`decode.rs` == `5824d7ce`) |
