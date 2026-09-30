# lane-av1422ctattr — the seed's 202 is an INTRABC frame copy, not a `dc_top`; and the defect is FIXED, all seven cells byte-exact

> **NOTE ADDED BY MAIN LATER (2026-09-30).** This report DOES carry the fix (its own title says so), but the
> MERGE MESSAGE for `1686dc8a` did not: I wrote that message from the lane's short output, which described
> only the attribution and the rungs, so the merge reads as if the commit were instruments-only. Commit
> `b8eed69f` inside it actually replaced `if skip {` in `sub8_leaf_chroma422` with
> `if let Some(dv) = intrabc_dv.filter(|_| skip) { <bilinear frame copy at the DV> } else if skip {` -- the
> last 4:2:2 corpus divergence. libaom does this because `predict_inter_block_visit` runs
> `dec_build_inter_predictor` REGARDLESS of `skip_txfm` for an inter/intrabc block
> (`decodeframe.c:1040-1043`); only the residual read is skipped. Independently reproduced by mutation and
> re-measured in `lanes/av1422ctintrabc.report.md`.
> CONSEQUENCE, recorded because it bit the decision table: the census (`lanes/av1422census.report.md`) was
> taken 23 minutes BEFORE this merge (`cc9f2668`), so its verdicts for `Q_odd320x242`, `O_odd322x242`,
> `AB_inter_warp_odd` and `S_odd326x242_10b` are stale; `lanes/av1422census2.report.md` re-measures the
> whole table on the current tree.


**Outcome in one line: both instrument gaps are closed, the oracle's `202` at the
seed is attributed to a named call site (`dec_build_inter_predictor`, via
`predict_inter_block`, because the block is INTRABC and `av1_predict_intra_block`
is never called), and our line — `sub8_leaf_chroma422` testing `skip` before
`intrabc_dv` — is fixed, taking all four failing cells plus both controls plus the
reproducer to byte-exact on every plane of every decode frame.**

Tip: `main` = `0f9ea788`. Worktree `/home/tahinli/.cache/wt/av1422ctattr`, branch
`lane-av1422ctattr`. Three files changed: `crates/ec-av1/src/decode.rs` (the fix),
`scripts/instrument-aom-oracle.sh` (rungs 19/20/21),
`scripts/check-aom-oracle-rungs.sh` (their assertions).
`crates/ec-av1/src/stream.rs` carried the temporary 4:2:2 refusal bypass during
measurement and is **restored** (see §8/§9). No push, no merge, no rustfmt.

---

## 0. What the brief assumed, and what measurement says

| brief's premise | measured verdict |
|---|---|
| the oracle's chroma at the seed is a `dc_top` prediction | **REFUTED** — there is no DC variant: the block is INTRABC and no intra prediction runs at all (§4) |
| the block structure at the seed is unreadable (no root at `mi_row=20, mi_col 24..31`) | **REFUTED as a label-space problem** — libaom's `MI_SIZE` is **4**, not 8; the seed is `mi_row=40, mi_col=58` and both ladders already agreed there (§3) |
| the writer is a third path | **CONFIRMED** — intra block copy, `decodeframe.c:688` `dec_build_inter_predictor` (§4) |
| our line is the availability gate / the DC arithmetic | **REFUTED (again, now measured)** — our parse of the seed is bit-identical to the oracle's, symbol for symbol (§4) |
| the fix must be gated | the 4:2:2 defect is **not gateable from a committed test** (§8); the fix is verified by the six 4:2:2 corpus cells + the reproducer against aomdec |

---

## 1. Reproduction, by me, from scratch

Source (hand-built y4m; ffmpeg will not write a `C422` tag):

```
ffmpeg -v error -f lavfi -i testsrc2=size=320x242:rate=25 -frames:v 16 \
       -pix_fmt yuv422p -f rawvideo src.raw -y
# header "YUV4MPEG2 W320 H242 F25:1 Ip A1:1 C422\n", then "FRAME\n" + 320*242
# + 2*(160*242) bytes per frame   -> 2 478 215 B
```

Encode:

```
/home/tahinli/.cache/aom-oracle/build/aomenc --codec=av1 --profile=2 \
  --input-bit-depth=8 --bit-depth=8 --limit=16 --lag-in-frames=25 --auto-alt-ref=1 \
  --enable-global-motion=1 --pass=1 --cq-level=24 --threads=4 --kf-min-dist=0 \
  --kf-max-dist=999999 --width=320 --height=242 --cpu-used=0 src.y4m -o n.webm
ffmpeg -v error -i n.webm -c copy -f obu R422_320x242.obu -y
```

| artifact | bytes | sha256 |
|---|---|---|
| **`R422_320x242.obu` (mine)** | **20 733** | **`909e58db18fd4b7590501e2cecfbc090cde49eb26c9a42e15b1ef1fc265843d8`** |

Byte-identical to the predecessor's `MYREPRO`/`R422_320x242.obu` — the recipe is
deterministic and reproduces exactly.

First bad decode frame / byte, measured with `planecmp.py` + `cmp` on
`EC_AV1_FINAL_DUMP` decode-order dumps from both sides:

| cell | decode frames | first bad decode frame | first bad byte |
|---|---|---|---|
| **`R422_320x242` (mine, fresh)** | 17 | **0** | **103156** |
| `Q_odd320x242` (corpus) | 17 | **0** | **103156** |
| `O_odd322x242` | 17 | 0 | 106388 |
| `AB_inter_warp_odd` | 43 | 0 | 110876 |
| `S_odd326x242_10b` | 17 | 0 | 214112 |
| `R_odd322x240` (control) | 17 | — | — |
| `V_tile2x2_odd` (control) | 17 | — | — |

The predecessor's byte table is reproduced exactly.

**Oracle control.** I did not build on the shared oracle tree (three lanes were
running). I copied `~/.cache/aom-oracle/src` to
`/home/tahinli/.cache/aom-oracle-av1422ctattr/src` and configured a private build
(`-DENABLE_EXAMPLES=1` is required — `aomdec` is gated on `ENABLE_EXAMPLES`, not
`ENABLE_TOOLS`, in libaom's `CMakeLists.txt:456`). Its `aomdec` output is
**byte-identical to the shared one on all 17 frames** of `R422_320x242.obu`, and its
`EC_TRACE` stderr is byte-identical to the shared oracle's `EC_TRACE` stderr
(`cmp` clean, 11 136 lines). It stays byte-identical after my rungs are added, with
no env set. The oracle diff below is therefore a real oracle diff, not a rebuild
artefact.

---

## 2. Instrument gap 1 — CLOSED (three new oracle rungs, via the generator)

All three are in `scripts/instrument-aom-oracle.sh` (the source of truth) and
asserted in `scripts/check-aom-oracle-rungs.sh`.

### rung 19 `EC_PIB` — every `av1_predict_intra_block` call, before any early return

The function has three exits that print nothing: the palette early return, the
8-bit non-directional early return, and the high-bitdepth one. `EC_PREDOUT8` /
`EC_PRED` cover only the last two's bodies, so a region with no rung line is
consistent with three different writers. `EC_PIB` prints the call identity at the
prologue:

```
EC_PIB mi_row=%d mi_col=%d plane=%d row_off=%d col_off=%d txw=%d txh=%d mode=%d
      use_palette=%d filter_intra=%d bsize=%d px=%d py=%d hbd=%d
```

`EC_PIB=1`; optional `EC_PIBWIN="mi_row_lo:mi_row_hi:mi_col_lo:mi_col_hi"`
(any field `< 0` = any) to isolate a seed without dumping a frame.

### rung 20 `EC_DCIN` — the DC path's availability INPUTS and the variant id

`dc_predictor`'s four variants are selected by `(have_top, have_left)` and scaled by
`(n_top_px, n_left_px)`. `EC_PREDOUT8` printed neither, so "libaom used `dc_top`"
was an inference from the predicted number. `EC_DCIN` prints, at both the 8-bit and
the high-bitdepth non-directional sites, before the predictor call:

```
EC_DCIN mi_row mi_col plane row_off col_off txw txh mode
        have_top have_left n_top n_left atop aleft dcv
        up left cup cleft ss_x ss_y bsize
```

`dcv`: 0 = `dc` (both edges), 1 = `dc_top`, 2 = `dc_left`, 3 = `dc_128`. `atop`/
`aleft` are the two edge sums the DC is taken from; `up`/`left`/`cup`/`cleft` are
the raw availability bits, including `set_mi_row_col`'s per-plane
`chroma_*_available` narrowing (`av1_common_int.h:1367-1379`). So the variant is now
a printed field.

### rung 21 `EC_DP` — the branch `decode_token_recon_block` takes

```
EC_DP mi_row mi_col bsize inter chroma_ref skip_txfm bw bh planes is_intrabc
```

`decode_token_recon_block` has two arms (intra prints a rung, inter does not) and
the intra arm skips chroma on `plane && !xd->is_chroma_ref`. `EC_DP` names which.

### The seed's new rung output

`EC_DP=1 EC_PIB=1 EC_DCIN=1 EC_PIBWIN=39:43:56:60`, `Q_odd320x242.obu`, decode
frame 0 (frame boundary located by the second `EC_PART mi_row=0 mi_col=0 bsize=15`
SB root):

```
EC_DP mi_row=40 mi_col=58 bsize=2 inter=1 chroma_ref=1 skip_txfm=1 bw=2 bh=1 planes=3 is_intrabc=1
EC_DP mi_row=41 mi_col=58 bsize=2 inter=1 chroma_ref=1 skip_txfm=0 bw=2 bh=1 planes=3 is_intrabc=1
```

`bsize=2` is `BLOCK_8X4` — the shape we already knew. **`inter=1` and
`is_intrabc=1`**: these two leaves are **intra block copy**. And in the whole frame-0
window there is **no `EC_PIB` line at `mi_row=40`/`41, mi_col=58` at all** — i.e.
`av1_predict_intra_block` is never called for this block, on any plane. For
comparison, the neighbour group one column left does print:

```
EC_PIB mi_row=40 mi_col=56 plane=1 row_off=0 col_off=0 txw=4 txh=8 mode=2 use_palette=0 filter_intra=5 bsize=3 px=0 py=0 hbd=0
EC_PIB mi_row=42 mi_col=58 plane=0 row_off=0 col_off=0 txw=8 txh=8 mode=11 use_palette=0 filter_intra=5 bsize=3 px=0 py=0 hbd=0
```

The rung fires; it is silent exactly at the seed.

### Checker

`scripts/check-aom-oracle-rungs.sh <private src>` — exit 0, every assertion green,
including the four new ones:

```
ok   rung 19 EC_PIB install sites (av1_predict_intra_block prologue) 1
ok   rung 19 EC_PIB carries use_palette= (the early-return bit) 1
ok   rung 20 EC_DCIN install sites (8-bit + hbd non-directional) 2
ok   rung 20 EC_DCIN prints availability operands + dcv + availability bits 2
ok   rung 21 EC_DP install sites (decode_token_recon_block) 1
ok   rung 21 EC_DP carries is_intrabc= (the intra/inter branch bit) 1
ok   instrument-aom-oracle.sh derives the depth-correct, byte-checked rungs (base v3.13.3)
```

Idempotence: the generator run twice over the tree leaves both derived files
`cmp`-identical.

---

## 3. Instrument gap 2 — CLOSED, and the gap was a unit error

**libaom's `MI_SIZE` is 4, not 8.** `av1/common/enums.h:39-40`:

```c
#define MI_SIZE_LOG2 2
#define MI_SIZE (1 << MI_SIZE_LOG2)
```

So `mi_cols = (width+3)>>2 = 80` for a 320-wide frame and `mib_size = 128>>2 = 32`.
The tell in the ladder is that the SB roots land at `mi_col = 0, 32, 64` and
`mi_row = 0, 32` — not `0,16,32`. The predecessor read the ladder on an 8-px grid,
concluded "no root covers `mi_row=20, mi_col 24..31`", and attributed it to the
documented label-space trap. On the correct grid the seed is `mi_row=40, mi_col=58`
— which is **exactly the `mi=(40,58)` our own trace already printed**, because
ec-av1's `MI` is 4 too (`decode.rs:7355`). There was never a label mismatch.

Both ladders, frame 0, at the seed (oracle `EC_TRACE` vs our `EC_AV1_TRACE`):

| oracle | ours |
|---|---|
| `EC_PART_VAL mi_row=40 mi_col=56 bsize=9 value=3` | `TRACE partition_w32 mi=(40,56) ctx=3 value=3` |
| `EC_PART_VAL mi_row=40 mi_col=56 bsize=6 value=3` | `TRACE partition_w16 mi=(40,56) ctx=3 value=3` |
| `EC_PART_VAL mi_row=40 mi_col=56 bsize=3 value=0` | `TRACE partition_w8  mi=(40,56) ctx=0 value=0` |
| `EC_PART_VAL mi_row=40 mi_col=58 bsize=3 value=1` | `TRACE partition_w8  mi=(40,58) ctx=0 value=1` |
| `EC_ISTEP mi_row=40 mi_col=58 name=skip val=1` | `TRACE sub8 skip mi=(40,58) ctx=1 value=1` |
| `EC_ISTEP mi_row=41 mi_col=58 name=skip val=0` | `TRACE sub8 skip mi=(41,58) ctx=1 value=0` |

Structure, skip bits and labels agree exactly. **Candidate (c) — "our parse of the
block structure is wrong at the seed" — is refuted.**

The oracle's ladder is not incomplete: it covers the seed. What it could not
express was *which arm wrote the block*, and `EC_PIB`/`EC_DP` (rung 19/21) supply
that. `EC_TRACE`'s own 4 498 `EC_PART_VAL` entries do include the seed's whole
ancestry; the ladder was read in the wrong unit.

---

## 4. THE ATTRIBUTION: which path wrote 202

### The chain, with every link printed

Oracle, `Q_odd320x242.obu`, decode frame 0, `EC_TRACE_MODE_STEP=1 EC_DP=1`:

```
EC_ISTEP mi_row=40 mi_col=58 name=skip   val=1 rng=44808
EC_ISTEP mi_row=40 mi_col=58 name=cdef   val=0 rng=44808
EC_ISTEP mi_row=40 mi_col=58 name=dq     val=37 rng=44808
EC_ISTEP mi_row=40 mi_col=58 name=intrabc val=1 rng=39264
EC_DV     mi_row=40 mi_col=58 dv_col=0 dv_row=-1024 rng=40116
EC_DP     mi_row=40 mi_col=58 bsize=2 inter=1 chroma_ref=1 skip_txfm=1 bw=2 bh=1 planes=3 is_intrabc=1
EC_ISTEP mi_row=41 mi_col=58 name=intrabc val=1 rng=47584
EC_DV     mi_row=41 mi_col=58 dv_col=0 dv_row=-1024 rng=53388
EC_DP     mi_row=41 mi_col=58 bsize=2 inter=1 chroma_ref=1 skip_txfm=0 bw=2 bh=1 planes=3 is_intrabc=1
```

Ours, the same block, `EC_TRACE_MODE_STEP=1`:

```
EC_IMODE mi_row=40 mi_col=58 fn=sub8 rng=51276
EC_ISTEP mi_row=40 mi_col=58 name=skip   val=1 rng=44808
EC_ISTEP mi_row=40 mi_col=58 name=cdef   val=0 rng=44808
EC_ISTEP mi_row=40 mi_col=58 name=dq     val=0  rng=44808     <- see note
EC_ISTEP mi_row=40 mi_col=58 name=intrabc val=1 rng=39264     <- IDENTICAL
EC_DV     mi_row=40 mi_col=58 dv_col=0 dv_row=-1024 rng=40116  <- IDENTICAL
EC_IMODE mi_row=41 mi_col=58 fn=sub8 rng=40116
EC_ISTEP mi_row=41 mi_col=58 name=intrabc val=1 rng=47584     <- IDENTICAL
EC_DV     mi_row=41 mi_col=58 dv_col=0 dv_row=-1024 rng=53388  <- IDENTICAL
```

(Note: our `dq` line prints a placeholder `0`, not the coded `base_q_idx` — that
print is our own and is not a signal value; `rng` agrees on every real symbol.)

**Our entropy parse at the seed is bit-identical to the oracle's.** The predecessor's
"parse in sync" finding is confirmed at symbol granularity: same `use_intrabc`, same
DV, same rng. So the divergence is purely in what is *reconstructed*.

### The writer

`read_intra_frame_mode_info` returns immediately after `read_intrabc_info`
(`av1/decoder/decodemv.c:936-939`):

```c
  if (av1_allow_intrabc(cm)) {
    read_intrabc_info(cm, dcb, r);
    if (is_intrabc_block(mbmi)) return;      /* no y_mode, no uv_mode, ... */
  }
```

so `mbmi->mode = DC_PRED` and `mbmi->uv_mode = UV_DC_PRED` are *assignment
defaults* set inside `read_intrabc_info` (`decodemv.c:756-760`), not a coded mode —
and, more importantly, `av1_predict_intra_block` is never reached. Then
`decode_token_recon_block`'s `else` arm runs
(`av1/decoder/decodeframe.c:1302` → `decode_token_recon_block` → `else { td->
predict_inter_block_visit(cm, dcb, bsize); … }`), i.e.

* **`predict_inter_block`, `av1/decoder/decodeframe.c:918`** — the loop hits
  `if (frame < LAST_FRAME) { assert(is_intrabc_block(mbmi)); assert(frame ==
  INTRA_FRAME); }` (`decodeframe.c:928-931`), so no `av1_setup_pre_planes` runs and
  the "reference" is the current frame; and
* **`dec_build_inter_predictor`, `av1/decoder/decodeframe.c:688`** →
  `av1_build_inter_predictor`, which **copies** from the current frame at
  `(mi_x + mv.col, mi_y + mv.row)`.

The `skip_txfm` bit changes nothing about this: the inter arm calls
`predict_inter_block_visit` unconditionally, and only the residual read is dropped
(`decodeframe.c:1225-1249`).

**Therefore the 202 is a bilinear copy of a flat 202 region at DV
`(0, -1024)` (1/8-pel, i.e. 128 luma rows above) — it is not a DC prediction, and
"which DC variant" is a category error: no DC variant exists on the oracle side for
this unit.** Every `EC_PIB`/`EC_PREDOUT8`/`EC_PRED` rung is silent there because the
function is not called.

Candidates from the brief, dispositioned:

* **(a) a different block/mode whose prediction is flat 202** — **refuted**: the
  block is the 8x4 leaf at `mi(40,58)` we already had (same labels, same partition,
  same skip), and it is not predicted at all.
* **(b) a path that is not `av1_predict_intra_block`** — **CONFIRMED**:
  `dec_build_inter_predictor` (decodeframe.c:688) under `predict_inter_block`
  (decodeframe.c:918), reached because the block is INTRABC.
* **(c) our block structure being wrong at the seed** — **refuted** by §3's
  side-by-side ladder.

### The refutations the brief told me not to re-derive — now measured, not inferred

* The seed's above row and left column are byte-identical on both sides, but that
  is irrelevant: neither side reads them.
* libaom's `chroma_left_available` for this leaf is indeed 1 (the `bw < 2` clause is
  dead at `bw = 2`), which is why "`dc_top`" was always going to be the wrong
  story: no DC path runs.
* `intra::dc`'s arithmetic is not involved.

---

## 5. OUR LINE, named, and the fix

`crates/ec-av1/src/decode.rs`, `sub8_leaf_chroma422` (the 4:2:2 sub-8 chroma unit
reader). Before:

```rust
let grids: (Grid, Grid) = if skip {
    … push_intra(plane, cpx, cpy, 4, uv_predict_mode, …)   // intra DC
} else if let Some(dv) = intrabc_dv {
    … mc::predict_with_filter(… dv …)                      // frame copy
```

The seed's leaf has `skip_txfm = 1` and `intrabc_dv = Some((0, -1024))`, so the
**`if skip` arm won and the frame copy below never ran**. Our prediction there:

```
@decode.rs:4641 OUR_PRED x=116 y=160 plane=1 side=4 side=4 mode=0 ad=0 ft=0 sum=2912 row0=[182,182,182,182]
```

182 = `(808 + 644 + 4)/8` — our DC, from an edge pair that the oracle never read.
The sibling leaf at `mi(41,58)` has `skip_txfm = 0`, takes the copy arm, and is
byte-identical on both sides — which is exactly the predecessor's §6 observation
that "the second unit is byte-identical on both sides".

The fix mirrors `sub8_leaf_chroma444`, which already orders its arms this way
(`intrabc_dv.filter(|_| skip)` first, lane-av1ibcskip2):

```rust
let grids: (Grid, Grid) = if let Some(dv) = intrabc_dv.filter(|_| skip) {
    // frame copy at the DV, no coefficient, then the armed-slot clear
} else if skip {
    … unchanged …
} else if let Some(dv) = intrabc_dv {
    … unchanged …
```

The armed-`intrabc_chroma_tx` clear at the end of the new arm is **load-bearing**,
not decoration: without it the previously-exact controls `R_odd322x240` and
`V_tile2x2_odd` broke and `S_odd326x242_10b` regressed 1052 → 22 915 (§7 records
that intermediate measurement). A skipped leaf reads no chroma coefficient, so the
inherited chroma `tx_type` must be dropped unconsumed, exactly like every other skip
route.

After, our prediction at the seed:

```
@decode.rs:4641 OUR_PRED x=116 y=160 plane=1 side=4 side=4 mode=0 ad=0 ft=0 sum=3232 row0=[202,202,202,202]
```

---

## 6. Per-cell, per-plane, per-decode-frame measurement

`FINAL` (post-filter) decode-order dumps from both sides, wrong-SAMPLE counts
(`planecmp.py`). **Before** = the lane baseline; **after** = with the fix.

| cell | frames | before Y/U/V | after Y/U/V | after total |
|---|---|---|---|---|
| `R422_320x242` (mine) | 17 | 0 / **6 337** / **8 398** | 0 / **0** / **0** | **0 (EXACT)** |
| `Q_odd320x242` | 17 | 0 / **9 603** / **14 456** | 0 / **0** / **0** | **0 (EXACT)** |
| `O_odd322x242` | 17 | 0 / **985** / **722** | 0 / **0** / **0** | **0 (EXACT)** |
| `AB_inter_warp_odd` | 43 | 0 / **3 705** / **4 366** | 0 / **0** / **0** | **0 (EXACT)** |
| `S_odd326x242_10b` | 17 | 0 / **301** / **751** | 0 / **0** / **0** | **0 (EXACT)** |
| `R_odd322x240` (control) | 17 | 0 / 0 / 0 | 0 / 0 / 0 | 0 (EXACT, held) |
| `V_tile2x2_odd` (control) | 17 | 0 / 0 / 0 | 0 / 0 / 0 | 0 (EXACT, held) |

Before, per decode frame (frame 0..9), for the record:

```
R422_320x242  0/369/376  0/165/258  0/358/445  0/400/500  0/344/437  0/418/503  0/459/608  0/416/570  0/386/516  0/371/517
Q_odd320x242  0/422/379  0/313/758  0/511/560  0/522/851  0/511/636  0/619/745  0/610/992  0/538/664  0/719/1191  0/606/981
O_odd322x242  0/32/16    0/59/36    0/59/32    0/69/40    0/69/32    0/51/41    0/54/35    0/55/40    0/73/46    0/60/54
AB_inter…     0/32/32    0/61/66    0/64/78    0/112/114  0/64/64    0/62/64    0/89/85    0/116/117  0/123/121  0/111/146
S_odd…10b     0/17/32    0/20/55    0/3/25     0/27/54    0/43/68    0/41/78    0/25/43    0/5/42     0/18/39    0/5/34
```

After: **all 17 (43) frames of all seven cells are byte-identical**, not merely
low-count.

### Oracle-flip control, after the fix

One bit flipped in the oracle's own frame-0 mid-frame byte (byte 125 830, U plane,
y=60 x=70), same comparator:

| | wrong bytes vs ours |
|---|---|
| unflipped oracle frame 0 | **0** |
| flipped oracle frame 0 | **1** (exactly that byte) |

The comparator bites on the new state: exactly one flipped bit produces exactly one
extra wrong sample, in the plane the flip landed in.

### 4:2:0 / 4:4:4 controls — the fix is chroma-format-local, as it should be

| cell | recipe | ours/oracle frames | exact |
|---|---|---|---|
| `C420` | testsrc2 320x242 yuv420p, the §1 recipe, `--profile=0` | 17 / 17 | **17/17** |
| `C444` | testsrc2 320x242 yuv444p, same | 17 / 17 | **17/17** |
| `sc_420` / `sc_444` | flat-rectangle screen content, `--deltaq-mode=3 --tune-content=screen` (194 INTRABC blocks in `sc_444`) | 16-17 | **exact** |
| `c_420` / `c_444` | same recipe at the original `testsrc2` source | 17 / 17 | **17/17** |

---

## 7. Class sweep

The class is *a SKIPPED intrabc block still predicts (libaom predicts regardless of
`skip_txfm`)*. Every arm in ec-av1 that can carry an intrabc DV, and its ordering:

| site | ordering | verdict |
|---|---|---|
| `sub8_leaf_chroma422` (4:2:2 sub-8 chroma) | was `if skip` first | **DEFECT — fixed here** |
| `sub8_leaf_chroma444` (4:4:4 sub-8 chroma) | `intrabc_dv.filter(\|_\| skip)` first (lane-av1ibcskip2) | correct |
| `decode_leaf_rect8`'s 4:2:0 group tail | skip test is INSIDE the intrabc arm (`if leaf_skips[1]`) | correct |
| `decode_leaf_split4`'s 4-leaf group tail (26707) | `if leaf_skips[3] { intra } else if last_intrabc { copy + chroma coeffs }` — the intrabc arm does **not** test `leaf_skips[3]` | **structurally suspect, NOT measured** |
| `decode_leaf8`'s square leaf (23792) | `LEAF8_INTRABC_HITS` path | correct (lane-kf900) |

For the suspect site I added a temporary env-gated probe at exactly
`leaf_skips[3] && last_intrabc.is_some()` and decoded every 4:2:0 / 4:4:4 cell I
could build (6 encodes: `c_420`, `c_444`, `sc_420`, `sc_444`, `sc_420deltaqmodetune`,
`sc_444deltaqmodetune`) plus all seven 4:2:2 cells: **0 hits on all 13**. So it is
structurally reachable but unexercised by any material in hand. I did **not** change
it — changing an unexercised arm on reasoning alone is exactly how the two controls
in §6 got broken once already. The probe is removed from the tree. Building a
witness for it is the named follow-up (§10).

The intermediate measurement that pinned the armed-slot requirement, kept because it
is the evidence for that part of the fix:

| cell | baseline | new arm, no armed-slot clear | new arm + clear |
|---|---|---|---|
| `Q_odd320x242` | 24 059 | 10 847 | **0** |
| `R422_320x242` | 14 735 | 10 687 | **0** |
| `AB_inter_warp_odd` | 8 071 | 0 | **0** |
| `O_odd322x242` | 1 707 | 3 055 (worse) | **0** |
| `S_odd326x242_10b` | 1 052 | 22 915 (much worse) | **0** |
| `R_odd322x240` (control) | 0 | 4 806 (BROKEN) | **0** |
| `V_tile2x2_odd` (control) | 0 | 0 | **0** |

---

## 8. Regression

`cargo test --release -p ec-av1 --lib -- 420 422 444 lossless warp intra --skip
bitrate_target_lands_within_5_percent_over_48_frames` on this lane tree at `0f9ea788`
+ this lane's diff, `CARGO_TARGET_DIR=/home/tahinli/.cache/tgt/av1422ctattr`,
`EC_AV1_AOMDEC=/home/tahinli/.cache/aom-oracle/build/aomdec`,
`EC_NOMEMGUARD=1`. `/tmp` was checked first (13 G of 16 G free, `/tmp/ec-av1-*`
cleared) so no gate can fail with `Os { code: 122, kind: QuotaExceeded }`.

**`test result: ok. 181 passed; 0 failed; 2 ignored; 0 measured; 618 filtered out;
finished in 223.26s`**

That is **the same count as `lanes/av1422ctrigger` §8's baseline (181 / 0 / 2)** and
as `lanes/av1422llodd` §8's, so nothing moved. No failure was a `/tmp`-full
artefact (`/tmp` had 13 G of 16 G free with `/tmp/ec-av1-*` cleared; the run was
green on the first attempt — no red-then-green sequence to explain).

For completeness, the same command **with the 4:2:2 refusal bypass still patched
in** (i.e. before the guard was restored) gives
`175 passed; 6 failed; 2 ignored`, and the six failures are exactly the six
`stream::tests::*` gates that assert the 4:2:2 refusal is present:
`a_non_420_subsampled_sequence_header_is_refused_by_name`,
`the_pinned_422_bigblock_witnesses_are_present_and_refuse_by_name`,
`the_pinned_422_intrabc_sb128_strip_witnesses_refuse_by_name`,
`the_pinned_422_lossless_inter_witnesses_are_present_and_refuse_by_name`,
`the_pinned_422_lr_off_witness_is_present_and_refuses_by_name`,
`the_pinned_422_residual_compound_warp_witness_is_present_and_refuses_by_name`.
That is the direct measurement of §8's gate-ability claim: the only thing standing
between this tree and those six gates is one `if` at `stream.rs:1803`.

---

## 9. Gate-ability: NO. Stated plainly, not substituted.

4:2:2 is refused at the **sequence header** —
`crates/ec-av1/src/stream.rs:1803`, `if seq.subsampling_x != seq.subsampling_y`.
**No committed fixture and no committed test can reach this code.** Every
measurement in §1 and §6 required patching that guard out; it is restored
(`stream.rs:1803` reads `if seq.subsampling_x != seq.subsampling_y {`) and
`git status` shows only the three files listed at the top.

What a future lane that lifts the refusal would need, now measured and reproducible:

* `R422_320x242.obu`, 20 733 B, `sha256 909e58db18fd4b7590501e2cecfbc090cde49eb26c9a42e15b1ef1fc265843d8`,
  with the full §1 recipe — the first 4:2:2 artifact in this line that is both
  buildable from scratch and hash-stable.
* a per-decode-frame, per-plane byte-exactness gate (`planecmp.py`) plus the
  oracle-flip control in §6.
* rungs 19/20/21 (`EC_PIB`, `EC_DCIN`, `EC_DP`) in `scripts/instrument-aom-oracle.sh`,
  asserted in `scripts/check-aom-oracle-rungs.sh` — without them this defect was
  three lanes deep, and the `MI_SIZE` unit error cost a fourth.

I am not offering a source-scan substitute, and I am not committing the bypass.

---

## 10. Handover

1. **`decode_leaf_split4`'s 4-leaf group tail** (`decode.rs:26707`) is the one
   unrepaired arm of this class: its intrabc arm never tests `leaf_skips[3]`, so a
   skipped intrabc chroma-reference leaf there would predict DC *and* read chroma
   coefficients the oracle does not read. It has zero hits on every cell I could
   build. It needs a witness, not a patch.
2. **libaom's `MI_SIZE` is 4.** Any past or future ladder read on an 8-px grid is
   wrong by a factor of two and will produce phantom "uncovered region" findings.
   `grep "define MI_SIZE" av1/common/enums.h` before believing one.
3. **Do not re-chase**: the header fields, the post-recon filters, the entropy parse
   (bit-identical at the seed), `intra::dc`'s arithmetic, `PlaneBuf::edges`' three
   missing terms, the `& !1` anchor mask, the palette colour maps, odd/even geometry.
4. `EC_DCIN`'s `atop`/`aleft` print 0 on every line I captured, because
   `av1_predict_intra_block` is called with `ref == dst` in the decoder and the DC
   operands are read from `ref` before the predictor writes. It is still the right
   place for them (that is where the availability lives), but a future lane reading
   `atop`/`aleft` should not be surprised by zeros; the oracle's post-filter
   neighbour dumps are the source for edge sums.

---

## 11. Handover hygiene

* Lane tree `/home/tahinli/.cache/wt/av1422ctattr`, branch `lane-av1422ctattr`.
  `git status --porcelain`: `M crates/ec-av1/src/decode.rs`,
  `M scripts/check-aom-oracle-rungs.sh`, `M scripts/instrument-aom-oracle.sh`,
  `?? lanes/av1422ctattr.report.md`. **`crates/ec-av1/src/stream.rs` is restored**
  — line 1803 reads `if seq.subsampling_x != seq.subsampling_y {` again — and the
  temporary class-sweep probe is gone from `decode.rs`.
* Primary checkout `/home/tahinli/Documents/Code/Rust/edith_codecs`:
  `git status --porcelain` **empty**. No relative-path leak. All edits used absolute
  paths inside the worktree.
* No push, no merge, no rustfmt.
* The private oracle copy `/home/tahinli/.cache/aom-oracle-av1422ctattr` is outside
  the repo; the shared `~/.cache/aom-oracle/src` was **not** written to (three lanes
  were running against it).
* The 4:2:2 sequence-header bypass is **not** committed anywhere.
* Measurement scripts and both source sets live outside the repo, in
  `/home/tahinli/.cache/lane-av1422ctattr/`.