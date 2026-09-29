# lane-av1txsizeaudit — every transform-size derivation in `crates/ec-av1/src/**`, against libaom's actual rule

Base: `main` `9b2f6c9d` (the tip `main` had when this lane started) **plus** the
`lane-av1ibc128arm` fix `4bfe8d8e` replayed on top (rebased clean as
`2fcc577a`), because the audit subject must be the tree the fix will land on and
the invariant gate must be runnable. Branch `lane-av1txsizeaudit`, worktree
`/home/tahinli/.cache/wt/av1txsizeaudit`. Oracle `~/.cache/aom-oracle`
(aom `v3.13.3-7-g9bb526a`), read-only.

**Verdict: 0 transform-size MISMATCH. 2 non-transform-size MISMATCH (context /
plane-extent geometry, one of them a sibling of the fixed arm). 3 UNVERIFIED
open cells, all already named in `lanes/av1444rect.report.md`. ~100 MATCHES.**
The `cu_tx = 32 << ss_x` rule is refuted twice over — by a C probe linked
against the oracle's own `libaom.a`, and by the oracle's `EC_TRACE_COEFF` on a
4:4:4 `--sb-size=128` stream.

---

## 0. The rule, restated with citations

libaom's chroma transform size is a **three-step table chain**, not arithmetic
on the subsampling:

```c
/* av1/common/blockd.h:1372-1379  (read in the oracle tree) */
static inline TX_SIZE av1_get_max_uv_txsize(BLOCK_SIZE bsize, int subsampling_x,
                                            int subsampling_y) {
  const BLOCK_SIZE plane_bsize =
      get_plane_block_size(bsize, subsampling_x, subsampling_y);
  assert(plane_bsize < BLOCK_SIZES_ALL);
  const TX_SIZE uv_tx = max_txsize_rect_lookup[plane_bsize];
  return av1_get_adjusted_tx_size(uv_tx);
}
```

```c
/* av1/common/blockd.h:1361-1370 */
static inline TX_SIZE av1_get_adjusted_tx_size(TX_SIZE tx_size) {
  switch (tx_size) {
    case TX_64X64:
    case TX_64X32:
    case TX_32X64: return TX_32X32;
    case TX_64X16: return TX_32X16;
    case TX_16X64: return TX_16X32;
    default: return tx_size;
  }
}
```

`av1_get_adjusted_tx_size` takes **no subsampling argument**. The 64→32 step is
unconditional whenever the plane block's `max_txsize_rect_lookup` entry has a
64 axis. Tables: `av1_ss_size_lookup` at `av1/common/common_data.c:19-43`,
`max_txsize_rect_lookup` at `av1/common/common_data.h:126-147`.

**The two structural facts that decide every row below:**

1. `plane_bsize = get_plane_block_size(bsize, ss_x, ss_y)` halves the width by
   `ss_x` and the height by `ss_y` **independently**. So the "chroma extent" is
   a per-axis `>> ss`, never a `/2` and never a `<< ss`.
2. Because the adjustment is per-axis and fires on *any* 64 axis, **a
   per-axis `.min(32)` on an already-subsampled chroma extent is an exact
   transcription of `av1_get_adjusted_tx_size` over the entire `TX_SIZE`
   domain** — not a blanket clamp. Check: `(64,64)|(64,32)|(32,64) → (32,32)`,
   `(64,16) → (32,16)`, `(16,64) → (16,32)`, and every other shape is already
   ≤ 32 on both axes. This is why the `.min(32)` sites in this crate are right
   and why the two rect rows that *preserve* a 16 (`TX_64X16 → TX_32X16`) are
   right too.

### 0.1 Measured ground truth (not reconstructed from memory)

`~/.cache/txaudit/probe.c` linked against `~/.cache/aom-oracle/build/libaom.a`,
calling `get_plane_block_size` → `max_txsize_rect_lookup` →
`av1_get_adjusted_tx_size` for all 22 block sizes × 4 `(ss_x, ss_y)`. Output in
`~/.cache/txaudit/out.txt`; table rendered in §1.0 below. Highlights:

```
DIRECT bsize=BLOCK_128X128 ss=00 plane_bsize=BLOCK_128X128 max_rect=64x64 adjusted=3 (32x32)
DIRECT bsize=BLOCK_128X128 ss=11 plane_bsize=BLOCK_64X64    max_rect=64x64 adjusted=3 (32x32)
DIRECT bsize=BLOCK_64X64    ss=00 plane_bsize=BLOCK_64X64    max_rect=64x64 adjusted=3 (32x32)
```

`av1_get_max_uv_txsize(BLOCK_128X128, 0, 0) == TX_32X32`, i.e. **`32` at every
subsampling, never `64` at 4:4:4.**

### 0.2 Oracle runtime confirmation (independent of this crate's counters)

4:4:4, `--sb-size=128`, `--limit=3`, `--cq-level=62`, gradient y4m
(`~/.cache/txaudit/grad444.obu`). Full `EC_TRACE_COEFF` histogram over all
3 frames:

| plane | tx=0 (4x4) | tx=1 (8x8) | tx=2 (16x16) | **tx=3 (32x32)** | **tx=4 (64x64)** | tx=5 (4x8) | tx=6 (8x4) | tx=7 (8x16) | tx=12 (64x32) | tx=13 (4x16) | tx=14 (16x4) | tx=17 (16x64) | tx=18 (64x16) |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| luma  | 420 | 186 | 75 | 13 | **8** | 130 | 74 | 14 | **2** | 31 | 0 | 0 | 0 |
| chroma| 240 | 179 | 80 | 49 | **0** | 134 | 88 | 25 | **0** | 32 | 0 | **0** | **0** |

**Luma reads TX_64X64 (8) and TX_64X32 (2). Chroma reads neither, and no
64-axis chroma shape at all.** That is the refutation as a runtime artifact.

And the mu-chunk structure, verbatim from the trace:

```
EC_COEFF plane=0 row=0 col=0  tx_size=4      <- luma TX_64X64
EC_COEFF plane=1 row=0 col=0  tx_size=3      <- U  TX_32X32
EC_COEFF plane=1 row=0 col=8  tx_size=3      <- U  TX_32X32
EC_COEFF plane=1 row=8 col=0  tx_size=3      <- U  TX_32X32
EC_COEFF plane=1 row=8 col=8  tx_size=3      <- U  TX_32X32
EC_COEFF plane=2 row=0 col=0  tx_size=3      <- V  TX_32X32
EC_COEFF plane=2 row=0 col=8  tx_size=3
EC_COEFF plane=2 row=8 col=0  tx_size=3
EC_COEFF plane=2 row=8 col=8  tx_size=3
EC_COEFF plane=0 row=0 col=16 tx_size=4      <- next 64x64 mu chunk
```

One TX_64X64 luma unit, then **FOUR** TX_32X32 chroma units per plane per
64x64 mu chunk, plane-major (all U, then all V), at `(row,col)` step 8 mi
(= 32 px / `MI`). The luma step is 16 mi (= 64 px). Under the ticket's rule
`cu_tx = 32 << ss_x`, the 4:4:4 chroma unit would have been 64 and each chunk
would have held **one** unit; libaom's trace says four of 32.

---

## 1. The table

Line numbers are the current worktree (`decode.rs` = 55960 lines). "adj step"
= does the site carry `av1_get_adjusted_tx_size`'s unconditional 64→32? `n-a`
= luma, plane dimension, mi coordinate, buffer stride, or scan/eob corner —
i.e. not a chroma transform size at all.

### 1.0 Ground truth `av1_get_max_uv_txsize` (measured, §0.1)

| block | ss_x ss_y | plane bsize | `max_txsize_rect_lookup` | after `av1_get_adjusted_tx_size` |
| --- | --- | --- | --- | --- |
| 4x4 | 00 / 01 / 10 / 11 | 4x4 | 4x4 | 4x4 |
| 4x8 | 00 | 4x8 | 4x8 | 4x8 |
| 4x8 | 01 / 11 | 4x4 | 4x4 | 4x4 |
| 4x8 | 10 | INVALID | (OOB) | (OOB) |
| 8x4 | 00 | 8x4 | 8x4 | 8x4 |
| 8x4 | 10 / 11 | 4x4 | 4x4 | 4x4 |
| 8x4 | 01 | INVALID | (OOB) | (OOB) |
| 8x8 | 00 | 8x8 | 8x8 | 8x8 |
| 8x8 | 01 | 8x4 | 8x4 | 8x4 |
| 8x8 | 10 | 4x8 | 4x8 | 4x8 |
| 8x8 | 11 | 4x4 | 4x4 | 4x4 |
| 8x16 | 00 | 8x16 | 8x16 | 8x16 |
| 8x16 | 01 | 8x8 | 8x8 | 8x8 |
| 8x16 | 11 | 4x8 | 4x8 | 4x8 |
| 8x16 | 10 | INVALID | (OOB) | (OOB) |
| 16x8 | 00 | 16x8 | 16x8 | 16x8 |
| 16x8 | 10 | 8x8 | 8x8 | 8x8 |
| 16x8 | 11 | 8x4 | 8x4 | 8x4 |
| 16x8 | 01 | INVALID | (OOB) | (OOB) |
| 16x16 | 00 | 16x16 | 16x16 | 16x16 |
| 16x16 | 01 | 16x8 | 16x8 | 16x8 |
| 16x16 | 10 | 8x16 | 8x16 | 8x16 |
| 16x16 | 11 | 8x8 | 8x8 | 8x8 |
| 16x32 | 00 | 16x32 | 16x32 | 16x32 |
| 16x32 | 01 | 16x16 | 16x16 | 16x16 |
| 16x32 | 11 | 8x16 | 8x16 | 8x16 |
| 16x32 | 10 | INVALID | (OOB) | (OOB) |
| 32x16 | 00 | 32x16 | 32x16 | 32x16 |
| 32x16 | 10 | 16x16 | 16x16 | 16x16 |
| 32x16 | 11 | 16x8 | 16x8 | 16x8 |
| 32x16 | 01 | INVALID | (OOB) | (OOB) |
| 32x32 | 00 | 32x32 | 32x32 | 32x32 |
| 32x32 | 01 | 32x16 | 32x16 | 32x16 |
| 32x32 | 10 | 16x32 | 16x32 | 16x32 |
| 32x32 | 11 | 16x16 | 16x16 | 16x16 |
| 32x64 | 00 | 32x64 | 32x64 | **32x32 (ADJ)** |
| 32x64 | 01 | 32x32 | 32x32 | 32x32 |
| 32x64 | 11 | 16x32 | 16x32 | 16x32 |
| 32x64 | 10 | INVALID | (OOB) | (OOB) |
| 64x32 | 00 | 64x32 | 64x32 | **32x32 (ADJ)** |
| 64x32 | 10 | 32x32 | 32x32 | 32x32 |
| 64x32 | 11 | 32x16 | 32x16 | 32x16 |
| 64x32 | 01 | INVALID | (OOB) | (OOB) |
| 64x64 | 00 | 64x64 | 64x64 | **32x32 (ADJ)** |
| 64x64 | 01 | 64x32 | 64x32 | **32x32 (ADJ)** |
| 64x64 | 10 | 32x64 | 32x64 | **32x32 (ADJ)** |
| 64x64 | 11 | 32x32 | 32x32 | 32x32 |
| 64x128 | 00 | 64x128 | 64x64 | **32x32 (ADJ)** |
| 64x128 | 01 | 64x64 | 64x64 | **32x32 (ADJ)** |
| 64x128 | 11 | 32x64 | 32x64 | **32x32 (ADJ)** |
| 64x128 | 10 | INVALID | (OOB) | (OOB) |
| 128x64 | 00 | 128x64 | 64x64 | **32x32 (ADJ)** |
| 128x64 | 10 | 64x64 | 64x64 | **32x32 (ADJ)** |
| 128x64 | 11 | 64x32 | 64x32 | **32x32 (ADJ)** |
| 128x64 | 01 | INVALID | (OOB) | (OOB) |
| 128x128 | 00 | 128x128 | 64x64 | **32x32 (ADJ)** |
| 128x128 | 01 | 128x64 | 64x64 | **32x32 (ADJ)** |
| 128x128 | 10 | 64x128 | 64x64 | **32x32 (ADJ)** |
| 128x128 | 11 | 64x64 | 64x64 | **32x32 (ADJ)** |
| 4x16 | 00 | 4x16 | 4x16 | 4x16 |
| 4x16 | 01 | 4x8 | 4x8 | 4x8 |
| 4x16 | 11 | 4x8 | 4x8 | 4x8 |
| 4x16 | 10 | INVALID | (OOB) | (OOB) |
| 16x4 | 00 | 16x4 | 16x4 | 16x4 |
| 16x4 | 10 | 8x4 | 8x4 | 8x4 |
| 16x4 | 11 | 8x4 | 8x4 | 8x4 |
| 16x4 | 01 | INVALID | (OOB) | (OOB) |
| 8x32 | 00 | 8x32 | 8x32 | 8x32 |
| 8x32 | 01 | 8x16 | 8x16 | 8x16 |
| 8x32 | 11 | 4x16 | 4x16 | 4x16 |
| 8x32 | 10 | INVALID | (OOB) | (OOB) |
| 32x8 | 00 | 32x8 | 32x8 | 32x8 |
| 32x8 | 10 | 16x8 | 16x8 | 16x8 |
| 32x8 | 11 | 16x4 | 16x4 | 16x4 |
| 32x8 | 01 | INVALID | (OOB) | (OOB) |
| 16x64 | 00 | 16x64 | 16x64 | **16x32 (ADJ)** |
| 16x64 | 01 | 16x32 | 16x32 | 16x32 |
| 16x64 | 11 | 8x32 | 8x32 | 8x32 |
| 16x64 | 10 | INVALID | (OOB) | (OOB) |
| 64x16 | 00 | 64x16 | 64x16 | **32x16 (ADJ)** |
| 64x16 | 10 | 32x16 | 32x16 | 32x16 |
| 64x16 | 11 | 32x8 | 32x8 | 32x8 |
| 64x16 | 01 | INVALID | (OOB) | (OOB) |

Two facts fall out and are used repeatedly below:

* **At 4:2:0 the chroma tx is ≤ 32 on both axes for every block size** — so a
  `.min(32)` there is a no-op and any *hardcoded 32* is a 4:2:0 accident that
  happens to be right.
* **At 4:2:0 the adjusted step DOES fire** for the 64x128 / 128x64 / 128x128
  roots (their plane block is BLOCK_64X64 / BLOCK_32X64 / BLOCK_64X32) — and it
  fires to the same 32x32 a per-axis `.min(32)` produces. That is precisely why
  the 4:2:0 path is byte-identical across every `.min(32)` vs `.min(64)` choice.
* `INVALID` = `av1_ss_size_lookup` maps that (shape, ss) to `BLOCK_INVALID`,
  which libaom indexes **out of bounds** in `max_txsize_rect_lookup`. That is
  libaom's own quirk, already modelled in this crate by the
  `chroma_422_oob` / `ChromaRect8x4` arms and measured at TX_4X8. Not this
  audit's rule; 4:2:2 is refused by name at the sequence header anyway.

### 1.1 `crates/ec-av1/src/decode.rs` — chroma transform sizes (the real audit)

| file:line | what it computes | libaom rule | adj step | evidence | verdict |
|---|---|---|---|---|---|
| `decode.rs:12100-12102` | `(64,32)\|(32,64)\|(32,32) => (32,32)`; `(64,16) => (32,16)`; `(16,64) => (16,32)` — the tiled chroma unit of `decode_rect_split` | `av1_get_adjusted_tx_size`, blockd.h:1361 | **yes** | measured table §1.0: 4:4:4 `64X32`→32x32, `64X16`→32x16, `16X64`→16x32. The rect rows *keep* a 16, so this is a genuine per-axis transcription, not `min(32)` on both | MATCHES — the reference transcription |
| `decode.rs:18907` | `(uw,uh) = (chroma_w.min(32), chroma_h.min(32))` + `nw,nh = chroma/uw` | same | **yes** | input is already chroma (`11492: chroma_w = bw >> ss_x`) — clamp AFTER subsample, correct order. 4:4:4 `64x32`→(32,32)/2 units, `64x16`→(32,16), `16x64`→(16,32). 4:2:0: inert (already ≤32) | MATCHES |
| `decode.rs:28702` | `(uw,uh) = (write_chroma_w.min(32), write_chroma_h.min(32))` in `read_inter_rect_chroma` | same | **yes** | same order; `side > 64` is consumed by the mu-chunk arm at `40357` first, so every reachable footprint ≤64 per axis | MATCHES |
| `decode.rs:9126-9127` | `uv_tx_w/h = ((w_mi*MI) >> ss).max(4).min(32)` — chroma tx published to the deblocker's `uv_tx_grid` | `av1_get_max_uv_txsize`; libaom reads `block_size_wide[plane_bsize]` at `reconintra.c:1936-1944` | **yes** | 4:2:0 128 root: `128>>1=64`→32 = libaom 32x32. 4:4:4 128 root: `128>>0=128`→32 = libaom `adjusted(TX_64X64)`=32x32. 4:4:4 `64x16`: →(32,16) = libaom TX_32X16. 4:4:4 `16x64`: →(16,32) | MATCHES (comment says "under 4:2:0" — **stale prose**, the code is ss-generic and right at 4:4:4) |
| `decode.rs:20632` | `let tx = if side >= 64 { 32 } else { side }` → `chroma_tx`/`chroma_tx_h` in `decode_block` | `av1_get_max_uv_txsize` | **yes, this IS the step** | arm is `else if ss_x==0 && ss_y==0` → 4:4:4 only, where `plane_bsize == bsize`. `decode_block` is only called with square 8/16/32/64/128; substitute: 8→8, 16→16, 32→32, 64→32, 128→32 — every one equals the measured 4:4:4 column | MATCHES |
| `decode.rs:38686` | same expression → `chroma_tx`/`chroma_set`/`scan` in `decode_inter_block` | same | **yes** | guard `ss_x==0 && ss_y==0 && strip_chroma.is_none()`. A rect strip is diverted at `40409` to `read_inter_rect_chroma`, which recomputes per-axis, so the square is never applied to a rect | MATCHES |
| `decode.rs:22086` | `chroma_tx = if lossless_frame { 4 } else { 32 }` in `decode_block_128rect` | same | **yes (hardcoded)** | block sizes are **only** `(128,64)` and `(64,128)` (callers `33214/33220/33337`; 128x128 goes to `decode_block`). Measured: both are **32x32 at all four (ss_x,ss_y)**. Not merely inert-correct: the per-axis consumer at `22113-22114` is `(64>>ss_x)/chroma_tx × (64>>ss_y)/chroma_tx` = 2×2 = four units at 4:4:4, 1×1 at 4:2:0 | MATCHES |
| `decode.rs:14115`, `40136`, `41923`, `42900` | `let cu_tx = 32usize;` — the chroma unit of one 64x64 mu chunk (intra-BC 128 root / single-ref / compound / **the fixed intra-in-inter arm**) | `av1_get_max_uv_txsize` | **yes** | `side > 64` guard ⇒ BLOCK_128X128/128X64/64X128 only. Measured: 32x32 at every subsampling. The step is carried in the *division* below, not the literal | MATCHES (4 sites) |
| `decode.rs:14130-14131`, `14365-14366`, `40149-40150`, `41936-41937`, `42921-42922` | `chunk_chroma_w/h = (64usize) >> ss_x/ss_y` | `block_size_wide[plane_bsize]`, reconintra.c:1943 | n-a (plane block) | per-axis: 4:2:0 32x32, 4:2:2 32x64, 4:4:4 64x64 | MATCHES |
| `decode.rs:14136-14137`, `14359-14360`, `40164-40165`, `41951-41952`, `42907-42908` | `unit_luma_w/h = cu_tx << ss_x/ss_y` — the unit's LUMA footprint in px | `mi_size_wide[plane_bsize] >> ss_x` inverted | n-a (coord) | `32<<0=32` (8 mi) at 4:4:4, `32<<1=64` (16 mi) at 4:2:0. **This is a `<< ss` on a LUMA span, not on a tx size** — the ticket conflated the two | MATCHES |
| `decode.rs:40151-40152`, `41938-41939`, `42923-42924`, `22113-22114`, `22214-22215` | `units_w/h = chunk_chroma_w/h / cu_tx`, `(64 >> ss)/chroma_tx` — units per plane per mu chunk | `decode_token_recon_block`'s chroma loop | **yes (in the division)** | 4:2:0 `32/32`=1 → one unit; 4:4:4 `64/32`=2 → **2×2 = FOUR units**; matches the oracle trace in §0.2 exactly. Under the ticket's rule this would be 1 | MATCHES |
| `decode.rs:40450-40451`, `42243-42244` | `cu_tx = chroma_tx; mu_units_n = chroma_side / cu_tx` — the 4:4:4 `chroma_side > chroma_tx` arm | `av1_get_max_uv_txsize(BLOCK_64X64, 0, 0)` = `adjusted(TX_64X64)` = TX_32X32 | **yes (inherited)** | guard `ss_x==0 && ss_y==0 && chroma_side > chroma_tx` with `side > 64` already consumed ⇒ `side == 64`; `64/32 = 2` → four units per plane, plane-major | MATCHES |
| `decode.rs:14354`, `43652` | `cu = if lossless { 4 } else { 32 }` — the mu-chroma re-stamp's unit size | TX_32X32 / lossless TX_4X4 | **yes** | reachable only when `mu_chroma`, set exclusively by the TX_32X32 and lossless arms | MATCHES |
| `decode.rs:22072` | `logical_tx = if lossless_frame { 4 } else { 64 >> depth }` | `read_tx_size`, decodeframe.c:1117 | n-a (luma) | lossless returns TX_4X4 before the tree is consulted | MATCHES |
| `decode.rs:22077` | `coeff_tx_side = logical_tx.min(32)` | `av1_get_max_eob`, spec 5.11.40 — a 64-length axis codes only its low 32 coefficients | n-a (luma eob corner) | paired with `default_scan(coeff_tx_side)` at `22083` | MATCHES |
| `decode.rs:18794` | `(luma_cw, luma_ch) = (bw.min(32), bh.min(32))` — the LUMA corner of a 64-axis strip | `av1_get_adjusted_tx_size` applied to luma `max_txsize_rect_lookup` | n-a (luma) | `64x32 → 32x32`, `64x16 → 32x16` — same per-axis law. The chroma twin is `18907` and is computed independently from `11492`; the two are never conflated | MATCHES |
| `decode.rs:45693` | `uv_tx = default_intra_tx_type(uv_predict_mode)` — a `TxType`, **not a size** | — | n-a | the *size* is the `bw,bh` handed to `read_coeffs_rect`; guard `chroma_444 && bw != bh` ⇒ plane block BLOCK_8X4/BLOCK_4X8, `max_txsize_rect_lookup` identity, one rect unit fills the plane block | MATCHES (not a site) |
| `decode.rs:47200` | `chroma_tx = if chroma_w == 8 { TX8 } else { TX4 }` in `decode_inter_block8` (`SIDE = 8` const) | `av1_get_max_uv_txsize(BLOCK_8X8, ss)` | n-a (no 64 axis reachable) | 4:2:0 plane block BLOCK_4X4 → TX4 ✓; 4:4:4 BLOCK_8X8 → TX8 ✓; 4:2:2 BLOCK_4X8 → `ChromaRect8x4`/`SCAN_4X8` via `read_inter_plane_rect` ✓. No 16-wide leaf exists in this fn | MATCHES |
| `decode.rs:14697` + `14805-14819` | `decode_block_rect` chroma: plane block → `TxbSet`/scan table | `av1_get_max_uv_txsize` | vacuous | called only with 32x16/16x32 strips (all 8 call sites); at 4:4:4 the largest plane block is BLOCK_32X16 = TX_32X16, identity under the adjustment. The `_ =>` arm **refuses** any other shape, so a future 64-axis caller cannot fall through | MATCHES |
| `decode.rs:15340` + `15414-15437` | `decode_leaf_rect` chroma (16x8/8x16 leaves) | same | vacuous | largest 16x8; 4:4:4 → 16x8 identity, 4:2:2 → 8x8 identity | MATCHES |
| `decode.rs:15932` + `15942-15972` | `decode_block_rect4` chroma (32x8/8x32 strips) | same | vacuous | 4:4:4 → BLOCK_32X8 = TX_32X8 identity, the `(32,8)` row at `15945`. The 4:2:2 `(4,32)` row is the BLOCK_INVALID OOB case (different rule). `_ =>` `unreachable!`s | MATCHES |
| `decode.rs:11541-11542` | `(8,4)/(4,8) => ChromaRect8x4` | `max_txsize_rect_lookup[BLOCK_8X4/4X8]` = TX_8X4/TX_4X8, identity | n-a | one rect unit fills the plane block | MATCHES |
| `decode.rs:44799-44811`, `46792-46809` | the 4:2:0 group-unit chroma arm (`B4`/`Chroma4`) in `decode_inter_sub8_split4` and `decode_inter_sub8_rect2` | `av1_get_max_uv_txsize(BLOCK_8X8, 1, 1)` = `max_txsize_rect_lookup[BLOCK_4X4]` = TX_4X4 | n-a | guarded `!chroma_444 && !chroma_422 && !intra_chroma` ⇒ **4:2:0 only**; at 4:4:4 the per-piece arm runs instead. Checked specifically because `B4` looks like a possible 4:4:4 miss — it is not | MATCHES |
| `decode.rs:11261`, `13124` | palette colour-index map extent `(bw>>ss_x).max(4), (bh>>ss_y).max(4)` | `av1_get_plane_block_size(bsize,1,1)`, common_data.c:19-43 — the PLANE BLOCK, not the transform | n-a | a palette block codes one colour-index sample per plane-block pixel regardless of how its coefficients are transformed. `.max(4)` is required: `BLOCK_16X4 → BLOCK_8X4` at 4:2:0, a plain halving read half the symbols | MATCHES (plane block) |
| `decode.rs:22673` | `(cw,ch) = (8>>ss_x, 8>>ss_y)` | plane block | n-a | per-axis plane extent | MATCHES (plane) |
| `decode.rs:21604-21606` | `plane_px = ((span_mi*MI)>>ss_x).max(4)`, `over_w_px/over_h_px` | `max_block_wide/high` in chroma px | n-a | frame-edge clip, not a transform | MATCHES (plane) |
| `decode.rs:29572` | `plane_dim = ((dim_mi*MI)>>ss).max(4)` | plane dimension | n-a | boundary test, not a transform | MATCHES (plane) |

### 1.2 `crates/ec-av1/src/decode.rs` — luma sites (in the taxonomy, not chroma sizes)

| file:line | what | libaom rule | adj step | verdict |
|---|---|---|---|---|
| `11404` | `depth_to_tx_wh`: `(bw.min(64), bh.min(64))` | `depth_to_tx_size`, blockd.h:1354-1359 (`max_txsize_rect_lookup[bsize]` then `sub_tx_size_map` × depth) | n-a (luma) | MATCHES — `min(64)` is the table's TX_64X64 ceiling; every caller's block is ≤64 so it is a no-op on the whole reachable set |
| `13166`, `22057`, `27463` | `tx_size_context_txfm_rect(..., bw.min(64), bh.min(64))` and the callee's re-clamp | `get_tx_size_context` compares `tx_size_wide[max_txsize_rect_lookup[bsize]]` | n-a (luma) | MATCHES — the clamp is inside the helper so all 7 callers inherit it; a 128-axis block is diverted to `decode_block_128rect` before reaching here |
| `20723`, `20787`, `27578`, `28070`, `28109-28110` | `max_tx = side.min(64)` (var-tx tree entry, `read_tx_size`, `read_block_tx_size`) | `max_txsize_rect_lookup` ceiling | n-a (luma) | MATCHES — 128 enters at 64 = `max_txsize_rect_lookup[BLOCK_128X128]`, sharing BLOCK_64X64's `tx_size_cat3` row |
| `27629` | `max_tx = blk_max_px.min(64)` in `txfm_partition_ctx_rect` | `txfm_partition_context`, `get_sqr_tx_size` | n-a (luma) | MATCHES — `max_idx = log2(max_tx)-2`; 128→4→idx 0, same as BLOCK_64X64 |
| `28563` | `tx = 64usize` under `bw.max(bh) > 64` in `read_block_tx_size_rect` | `max_txsize_rect_lookup[BLOCK_128X64/64X128]` = TX_64X64 | n-a (luma) | MATCHES |
| `13836-13837`, `14464-14465`, `16859-16860` | `bw.min(64) as u8, bh.min(64) as u8` — luma transform published to `tx_grid` | `set_txfm_ctxs(mbmi->tx_size)` | n-a (luma grid) | MATCHES — a 128 root's `mbmi->tx_size` is TX_64X64; `128.min(64)=64` |
| `13632`, `14081`, `40049`, `41789`, `42701` | `default_scan(tw.min(32))` / `default_scan(tx_px.min(32))` | `av1_scan_orders`, scan.c:1744-1762 — TX_64X64/64X32/32X64 **alias to `default_scan_32x32`** | n/a — table-boundary guard, *not* the adjusted step | MATCHES — load-bearing (a 128 root's luma leaf really is TX_64X64, asserted at `13706`) and correct, sourced independently. `default_scan` only builds 4/8/16/32 |
| `28925` | `(cw,ch) = (w.min(32), h.min(32))` in `read_inter_plane_rect` | `av1_get_max_eob` | n-a (eob corner) | MATCHES — `w,h` are the true rect transform dims; `rect_shape` preserves the nominal |
| `36015` | `tx_side = side.min(32)` in `read_inter_plane` | `av1_get_max_eob` | n-a (eob corner) | MATCHES — inert below 64, `extend_corner` re-widens |
| `494` | `cfl_allowed_px`: `(bw>>ss_x).max(4)==4 && (bh>>ss_y).max(4)==4` | `is_chroma_sub8_x/y` shape (blockd.h:1533) — a plane-block `== 4` test | n-a | MATCHES — 4:2:0 8x8 → 4x4 true; 4:4:4 8x8 → 8x8 false, which is the documented `wrong-alphabet-same-value` case |
| `37601-37602` | `overlap_above/left = write_{h,w}.min(64)/2` | `av1_build_obmc_inter_prediction`, reconinter.c 860/899 | n-a | MATCHES (prediction overlap, not a transform) |
| `19636` | `cfl_ac_q3_at`'s `(bw/2, bh/2)` | 2x2 luma average | n-a | MATCHES (per-sample CfL AC; the 4:4:4/4:2:2 forms are separate branches at `19585`/`19599`, so this arm is 4:2:0-only) |

### 1.3 `crates/ec-av1/src/decode.rs` — mi coordinates, plane origins, strides (listed, all n-a)

`9915-9916` `snap_x/snap_y = (1<<ss)-1` (a mi-coordinate de-offset mask, libaom
`set_mi_row_col`'s `& ss`; correctly 0 at 4:4:4) · `11490-11492`,
`13890-13899`, `14696-14697`, `15339-15340`, `15931-15932`, `18699-18700`,
`20804`, `22093-22094`, `22623`, `24568`, `25200`, `26340`, `33111-33112`,
`33230-33239` — plane origins and plane extents, all per-axis `>> ss` ·
`20805`+`20808` `chroma_side`/`chroma_height` (per-axis, the sibling pair
libaom's rect chroma needs) · `38655` `chroma_side = (side>>ss_x).max(side>>ss_y)`
(a **buffer stride**, the enclosing square; documented at `38645-38654`) ·
`12023-12024` `span = 4 << ss`, `12137-12138` `luma_span = uw << ss`,
`22236-22240`, `28762`, `36103-36105`, `36145-36146`, `36265-36306`,
`18007-18106`, `42982-42984` — the LUMA footprint of a chroma unit, and the
`ox << ss_x` / `oy << ss_y` pair-extent form. All MATCHES: plane/coord, not a
transform size. `30510-30511` is a frame dimension. `27357-27358`, `27371-27372`,
`31627-31628`, `31848` are plane/loop-filter spans. `29452` is an OBMC step.
`33111-33239` is plane bookkeeping. None of these is a transform size.

### 1.4 `crates/ec-av1/src/decode.rs` — the two non-transform MISMATCHes + the open cells

| file:line | what it computes | libaom rule | adj step | evidence | verdict |
|---|---|---|---|---|---|
| `decode.rs:9103` | `suppress_internal_lf_edges`: `(uv_w, uv_h) = ((w_mi*MI/2).max(4), (h_mi*MI/2).max(4))` — the chroma extent the deblocker is told the skipped block has, so no internal edge reads as a TU edge | libaom's analogue is `set_one_param_for_line_luma/_chroma` (`mbmi->skip_txfm && is_inter_block` ⇒ the block's own mode_lf_lut row 0, no internal TU edge); the port emulates it by publishing the block's own chroma extent | **no** — hardcoded `/2`, not `>> ss` | Its **sibling `fill_lf_grid_rect` at `9126-9127` was already repaired to `>> ss_x(fctx)`; this one was not.** Reachability: sole caller `43954-43956`, guard `skip_inter_128 = skip && is_inter && write_w.max(write_h) > 64` — a SKIP inter 128x128/128x64/64x128 block, fully reachable at **4:4:4** (only 4:2:2 is refused). At 4:2:0 `128/2 = 64` = `128>>1` ✓; at 4:4:4 the code yields **64** where the block's chroma extent is **128** | **MISMATCH** — charter C1 |
| `decode.rs:42974` | `let cu_around = neighbours.around_mi(unit_mi, unit_luma_w);` — the coefficient-context gather inside the **fixed** intra-in-inter mu walk | `get_txb_ctx_general`, txb_common.h:293-309: `txb_w_unit = tx_size_wide_unit[tx_size]` and `txb_h_unit = tx_size_high_unit[tx_size]` — **per axis** | n/a (context geometry, not a size) | `around_mi` (`9553`) takes ONE `side` and gathers `side/MI` cells in **both** axes. `around_mi_rect` (`9662`) is the same loop with the two extents split; with `w == h` it is byte-identical. The inter twin at `14191-14192` already calls `around_mi_rect(unit_mi, unit_luma_w, unit_luma_h)`, and the record at `42996+` is per-axis. `unit_luma_w != unit_luma_h` only at 4:2:2, where it over-gathers 8 mi of LEFT (or ABOVE) context libaom does not read. At 4:2:0 and 4:4:4 `unit_luma_w == unit_luma_h` ⇒ inert | **MISMATCH (4:2:2 only)** — charter C2 |
| `decode.rs:13361-13373` | `decode_intrabc_rect`: the LOSSY arm's `(px/2, py/2)` and `(bw/2, bh/2)` chroma origin/extent (the lossless arm beside it uses `>> ss_x/ss_y`) | `block_size_wide[plane_bsize]`, reconintra.c:1943 | n/a (plane extent) | At 4:2:0 the `/2` is exactly `>> ss` for both axes ✓. At **4:4:4** a lossy intrabc rect strip reads **half** the chroma plane block. The `/2` is guarded only by `lossless(fctx)` — the `>> ss` path is the *lossless* one, so the defect is on the lossy side, i.e. 4:2:0-shaped arithmetic on the arm 4:4:4 actually uses. Already a declared open cell (`lanes/av1444rect.report.md:258,343,373`) | **UNVERIFIED** — missing: a 4:4:4 **lossy** `use_intrabc` rect-strip fixture (0 of the pinned fixtures decode one) |
| `decode.rs:16558` | `decode_intrabc_owned_rect`: `(cw, ch) = (bw/2, bh/2)` — **no** `lossless` branch at all | same | n/a (plane extent) | Same defect with no scoping guard. Sole caller `18525`, reached only when `intrabc.is_some()` — a 4:4:4 screen-content key frame with a 64-axis intrabc strip | **UNVERIFIED** — same open cell, reached through a 64-axis rather than a 32-axis strip |
| `decode.rs:28453` | `(cw,ch) = (w.min(32), h.min(32))` in the `corner_scan` closure of a `#[test]` | `av1_get_max_eob` | n-a (test) | production twin is `28925`; the test asserts every rect shape the census lists has a table | MATCHES (test) |

### 1.5 `crates/ec-av1/src/qm.rs`

| file:line | what | libaom rule | adj step | evidence | verdict |
|---|---|---|---|---|---|
| `qm.rs:57` | `fn adjusted(w,h) -> (usize,usize)` — the qmatrix shape reuse | `av1_get_adjusted_tx_size`, blockd.h:1361-1370 | **yes — this IS it** | Exact five-arm transcription: `(64,64)\|(64,32)\|(32,64) → (32,32)`, `(64,16) → (32,16)`, `(16,64) → (16,32)`, else identity. Matches blockd.h arm for arm | MATCHES |
| `qm.rs` `shape_range` | the shapes `adjusted` is asked about | `av1_get_max_eob` 1024 cap | **yes** | covers the 14 own-storage shapes, so `adjusted` never misses a shape the encoder can produce | MATCHES |
| `qm.rs` `iwt_matrix` | column-major stride fed through the adjusted height | — | **yes** | the adjusted height is the stride, matching libaom's coefficient buffer | MATCHES |

### 1.6 `crates/ec-av1/src/transform.rs`

| file:line | what | libaom rule | adj step | evidence | verdict |
|---|---|---|---|---|---|
| `transform.rs` (4 decode-side sites) | `cols = w.min(32)`; the `i < 32` row gate; `t[cols..w].fill(0)`; `get_rect_tx_log_ratio` | `av1_get_max_eob`, spec 5.11.40 — a 64-axis transform codes only its 32x32 corner | **yes, per axis** | The per-axis `min(32)` is the adjusted step, applied unconditionally per axis, and is exact for all 19 shapes | MATCHES |
| `transform.rs` `Transform_Row_Shift` | the row-shift table, including every 64-axis row | `av1_scan_orders`, scan.c:1744-1762 | **yes** | 64x64/64x32/32x64 rows all shift as 32x32, matching libaom's alias | MATCHES |
| `transform.rs:1644`, `2088`, `2165` | encoder-side coded-corner clamps | same | **yes** | same rule on the write side | MATCHES |

### 1.7 `crates/ec-av1/src/quant.rs`

| file:line | what | libaom rule | adj step | evidence | verdict |
|---|---|---|---|---|---|
| `quant.rs` (4 sites) | `dq_denom` / `dq_denom_area`, keyed on the TRUE tx area `w*h` | `av1_get_tx_scale`, quant_common.c — keyed on `tx_size_2d` | **no, and correctly so** | 64x64 must give 4096→4, **not** 1024→2. libaom's `av1_get_tx_scale` uses the nominal size; the adjusted step is a *coefficient-buffer* concept, not a quantiser one | MATCHES |

### 1.8 `crates/ec-av1/src/tile.rs`

| file:line | what | libaom rule | adj step | evidence | verdict |
|---|---|---|---|---|---|
| `tile.rs:5490`, `5537`, `5504`, `5559`, `5593` | luma var-tx roots: `max_tx = side.min(64)`, `>> depth`, `/2` | `max_txsize_rect_lookup` + `sub_tx_size_map` | n-a (luma) | pure luma, no adjustment | MATCHES |
| `tile.rs:5380` | `coeff_side = tx.min(32)` | `av1_get_max_eob` | **yes** | the coded-position count, carrying the step | MATCHES |
| `tile.rs` `TxbSet::Luma64` | side 32, eob 1024 | `av1_get_max_eob` 1024 cap | **yes** | the 32x32 corner | MATCHES |
| `tile.rs` `TxbSet::Chroma4/8/16/32` | the chroma side = luma side / 2 | `av1_get_max_uv_txsize` at 4:2:0 | **yes, implicit** | reproduces the 4:2:0 column exactly including the 32 cap; the encoder writes 4:2:0 only (`encode.rs:2191-2192`, `encoder.rs:77`), so the `/2` is exact by construction | MATCHES |
| `tile.rs:5000-5004` | `palette_uv_side(side) = av1_get_plane_block_size(bsize,1,1)` floored at 4 | common_data.c:19-43 | n-a (plane block) | not a transform size; the `4 << ss`-free floor is the 4-px minimum | MATCHES (plane) |
| `tile.rs:3355-3359` | `tx_size_ctx(..., max_tx)` | `get_tx_size_context` | n-a (luma) | neighbour transform bookkeeping | MATCHES (luma) |

### 1.9 `crates/ec-av1/src/encode.rs` (ENCODER — a mismatch here is a rate/bitstream defect, not a decode defect)

| file:line | what | libaom rule | adj step | evidence | verdict |
|---|---|---|---|---|---|
| `encode.rs:5302-5304` | `c = side / 2; c.min(32)` — the only real chroma-tx derivation | `av1_get_max_uv_txsize` at 4:2:0 | **yes** — `.min(32)` IS the step | 4:2:0 only (`encode.rs:2191-2192` constrains the encoder to 4:2:0), so no site is reachable with a 64 chroma axis | MATCHES |
| `encode.rs:4777-4780` | `max_tx_depth(side)` | `MAX_VARTX_DEPTH` = 2 | n/a | `max_tx_depth(64) = 0` sits *below* libaom's 2 — a search restriction with no bitstream consequence (it costs bits, it does not desync) | MATCHES (encoder search) |
| `encode.rs:5256-5270` | `txfm_split_bits`: `max_tx = side.min(64)`, `max_tx/2` | `read_block_tx_size` | n/a (luma) | rate model | MATCHES (encoder) |
| `encode.rs:4777`, `5532-5543`, `5612-5613`, `6241-6256`, `6759-6762`, `6804-6808`, `7281-7285` | remaining luma var-tx / chroma-extent derivations | as above | n/a / vacuous | zero `<< ss` sites in the whole file; every chroma transform side is `side/2` at 4:2:0 | MATCHES |

### 1.10 `crates/ec-av1/src/cdf_state.rs`

| file:line | what | libaom rule | adj step | evidence | verdict |
|---|---|---|---|---|---|
| `cdf_state.rs` `TxbTables.side` | the entropy-context shape | `get_txsize_entropy_ctx`, entropy.h:172 — squares UP, **no** adjustment | **no, correctly** | `LumaRect32x8` gets side 16, not 32 | MATCHES |
| `cdf_state.rs` `eob_pt` table length | the CODED-position count | `av1_get_max_eob` | **yes** | `Luma64 → 1024` = the 32x32 corner | MATCHES |

Two different libaom rules, both transcribed correctly.

### 1.11 `crates/ec-av1/src/stream.rs`, `refusal_inventory.rs`, `gate_coverage.rs`, `census.rs`, `restoration.rs`, `examples/`

| file:line | what | verdict |
|---|---|---|
| `stream.rs:2458-2492` | source-scan test pinning `"let unit_luma_w = cu_tx << ss_x(fctx);"`, `"let (cw, ch) = (bw >> ss_x(fctx), bh >> ss_y(fctx));"`, and a `per_axis_units` closure `(64>>ss_x)/32 * (64>>ss_y)/32` asserting 1 / 2 / 4 | MATCHES — and it pins the rule **correctly**. All six pinned strings assert either a luma-grid conversion of a chroma unit or a plane extent; **none** asserts a subsampling-dependent chroma *transform* size. `per_axis_units` agrees with libaom. (Its `ss(1,0)` arm is dead — 4:2:2 is refused.) |
| `refusal_inventory.rs:1533-1668` | the single most direct pin of the rule in the crate: `chroma_w = bw >> ss_x` **then** `unit = (chroma_w.min(32), chroma_h.min(32))` | MATCHES — it pins the **correct composition order** (halve first, then cap per axis), the opposite of the wrong rule. Its 8-case walk is checked against the five-arm table. |
| `refusal_inventory.rs:1577` | source-scan pin of `"(bw >> ss_x(fctx), bh >> ss_y(fctx))"` | MATCHES (plane extent) |
| `refusal_inventory.rs:1654-1656` | a doc citing `av1_ss_size_lookup[BLOCK_8X16][1][1] = BLOCK_4X8 → max_txsize_rect_lookup[BLOCK_4X8] = TX_4X8` via `av1_get_max_uv_txsize` | MATCHES — measured: `BLOCK_8X16` at 4:2:0 → plane BLOCK_4X8 → TX_4X8 ✓ |
| `restoration.rs:439-440`, `531-542`, `1504` | `>> ss` on a plane dimension, an LR mi step, an LR stripe height | MATCHES (plane/coord — the charter's explicit non-sites) |
| `gate_coverage.rs`, `census.rs` | census of aomenc tool-FLAG **spellings** (`enable-tx64`, `enable-tx-size-search`, `enable-rect-tx`); `census.rs:198` lists the four `tx_size_cat*` symbol names | MATCHES — no transform-size site. **`enable-tx64` is an encoder switch, not a transform size, and carries no 64→32 step**; nothing here pins the wrong rule |
| `examples/decode_probe.rs:105-111`, `examples/syntax_census.rs:56-72`, `enc_probe.rs`, `dump_yuv.rs`, `alloc_probe.rs` | print counters / classify symbol names | MATCHES (no site) |
| `mc.rs`, `intra.rs`, `warp.rs`, `sequence.rs`, `superres.rs`, `bits.rs`, `msac.rs`, `compound.rs`, `motion.rs`, `motion_field.rs`, `speed.rs`, `par.rs`, `film_grain.rs`, `filter_search.rs`, `encoder.rs`, `frame.rs` | `frame.rs`'s two near-hits are the `tx_mode_select` header bit and `using_qmatrix`/`qm_y`/`qm_u` (a frame flag and matrix **levels**) | MATCHES — no transform-size site in any of them |

---

## 2. Verdicts

### 2.1 Transform sizes: 0 MISMATCH

Every site that can reach a 64 axis at 4:4:4 either carries
`av1_get_adjusted_tx_size` (as a `.min(32)` per axis, an explicit five-arm
table, a `if side >= 64 { 32 }`, or — in the mu-chunk walks — as a division
`chunk_chroma / cu_tx` that reads 2×2 = four units) or is provably never
reachable with one. The rule's central case — a 4:4:4 128x128 block coding
**four** 32x32 chroma units per 64x64 mu chunk — is implemented at four
independent sites (`14115`, `40136`, `41923`, `42900`) and confirmed against
the oracle's own trace (§0.2).

The luma `min(64)` and `min(32)` clamps are a **different** libaom rule
(`max_txsize_rect_lookup`'s ceiling and `av1_get_max_eob`'s 1024 cap, with
`av1_scan_orders` aliasing every 64-axis scan to `default_scan_32x32`) and are
all correct; the audit keeps them separate from the chroma rule so the two are
never conflated.

### 2.2 Two MISMATCHes, both non-transform — chartered, not fixed here

**C1 — `decode.rs:9103`, `suppress_internal_lf_edges`'s hardcoded `/2`.**
Ship a 4:4:4 + `--sb-size=128` **skipped inter 128 root** fixture. With it:
replace `(w_mi * MI / 2)` with `(w_mi * MI) >> ss_x(fctx)` and
`(h_mi * MI / 2)` with `(h_mi * MI) >> ss_y(fctx)` — the identical two-line
form its sibling `fill_lf_grid_rect` (`9126-9127`) already uses — then
red-before by reverting only the two shifts. The missing witness is the whole
blocker: a deblocker difference is not observable on any pinned 4:2:0 or
4:4:4 intra fixture, and per `lanes/av1tilerows.report.md:254-256` ("I could
not build a witness, and I will not ship an unvalidated change into a path with
zero coverage") an unproven change here would be the same mistake in reverse.
Consequence if shipped unfixed: at 4:4:4 a SKIP inter 128 root publishes a
chroma extent of 64 where libaom's block has 128, so the deblocker filters an
internal chroma edge at 64 that libaom's `set_one_param_for_line_*` suppresses.

**C2 — `decode.rs:42974`, `around_mi` where the arm's own twin uses
`around_mi_rect`.** This one *is* a one-line change and it is provably inert at
4:2:0 and 4:4:4 (`around_mi_rect(m, w, h)` with `w == h` is the same loop over
the same arrays — compare `9553-9579` with `9662-9686`; the identity set below
is the empirical check). It is still chartered rather than fixed, for two
reasons that the brief's own rule demands: (a) it is **4:2:2-only**, and 4:2:2
is refused by name at the sequence header, so no gate in the repo can be made
red — a fix I cannot prove red-before is exactly what the brief says not to
ship; (b) it is a *context-geometry* defect, not a transform size, so it is
outside the class this lane was chartered to fix. A 4:2:2 lane with
`EC_AV1_ALLOW_422_PROBE` (never committed) can land it in one line.

### 2.3 Three UNVERIFIED open cells (all pre-existing, all already named)

`decode.rs:13361-13373` and `decode.rs:16558`: the intra-BC rect-strip chroma
footprint is hardcoded `bw/2, bh/2` on the lossy side, where libaom's plane
block at 4:4:4 is the block itself. Both belong to the open cell recorded at
`lanes/av1444rect.report.md:258,343,373`; the 64-axis path (`16558`) has no
`lossless` scoping, which the report should be amended to say. **Missing for
both: a 4:4:4 lossy `use_intrabc` rect-strip fixture** — zero of the pinned
fixtures decode one. Decodes wrong if reached: half the chroma plane block of a
4:4:4 lossy intrabc strip.

### 2.4 Stale prose (not a defect)

`decode.rs:9123-9125` says `uv_tx_w`'s derivation is "under 4:2:0". The code is
subsampling-generic and **correct at 4:4:4**, where the adjusted step is the
load-bearing part. Worth a comment edit in the next lane that touches the file;
it is the kind of stale qualifier that is how the wrong rule got reintroduced
once already.

---

## 3. Refuted rules

### 3.1 The rule that was wrong

> **"`cu_tx` must be `32 << ss_x(fctx)`; at 4:4:4 `av1_get_max_uv_txsize`
> returns TX_64X64, so `cu_tx` is 64."**

**REFUTED.** `blockd.h:1371` (the ticket's own citation) is the *body* of
`av1_get_max_uv_txsize`, and its last line is
`return av1_get_adjusted_tx_size(uv_tx);`. `av1_get_adjusted_tx_size`
(`blockd.h:1361`) takes **no subsampling argument** and maps `TX_64X64`,
`TX_64X32`, `TX_32X64` to `TX_32X32` **unconditionally**. Measured against
libaom's own code, `av1_get_max_uv_txsize(BLOCK_128X128, 0, 0) == TX_32X32`.
The ticket read `max_txsize_rect_lookup[BLOCK_128X128] = TX_64X64` and stopped
one line short, annotating the adjustment as "adjusted → itself".

The refutation has two independent witnesses, both reproduced in this lane:

1. A C probe linked against `~/.cache/aom-oracle/build/libaom.a` calling the
   real three functions over all 22 block sizes × 4 `(ss_x, ss_y)`
   (`~/.cache/txaudit/probe.c`, output `out.txt`).
2. The oracle's own `EC_TRACE_COEFF` on a 4:4:4 `--sb-size=128` stream: luma
   reads `tx_size=4` (TX_64X64) eight times, chroma reads it **zero** times and
   reads **zero** 64-axis chroma shapes; every 64x64 luma unit is followed by
   exactly **four** `tx_size=3` (TX_32X32) chroma units per plane.

### 3.2 Where the wrong rule spread

| where | what | state |
|---|---|---|
| `lanes/av1tilerows.report.md:265-281` | the **origin**. §8 "Recommendation" states "`av1_get_max_uv_txsize(BLOCK_128X128, 0, 0)` is `max_txsize_rect_lookup[BLOCK_128X128]` = `TX_64X64` (adjusted → itself)" and recommends repairing the site to `cu_tx = 32 << ss_x(fctx)`, `luma_span = cu_tx << ss_x(fctx)` | **RETRACTED in place by this lane** — a `RETRACTED by lanes/av1txsizeaudit.report.md §3.1` blockquote now sits on item 3 and on the Recommendation, quoting the correction and the oracle measurement, so the claim cannot be lifted out of context again |
| a census item, and a charter I wrote from it | the same claim, propagated | never committed to `crates/ec-av1/src/**` |
| `crates/ec-av1/src/**` | `git log --all -S"cu_tx = 32 << ss" -- crates/ec-av1/src` → **no commits**; `-S"cu_tx = 32usize << ss"` → **no commits** | **never implemented.** The wrong rule never reached code. |
| the four live `cu_tx` sites (`decode.rs:14115`, `40136`, `41923`, `42900`) | all `let cu_tx = 32usize;` with the per-axis walk around them | **correct** |
| `crates/ec-av1/src/stream.rs:2458-2492` and `refusal_inventory.rs:1533-1668` | the two source-scan tests that pin chroma-size shape | both pin the rule **correctly** |

The wrong rule's most seductive property is that its 4:2:0 identity argument is
true: `32 << 1 == 64`, and at 4:2:0 the adjusted step is a no-op on a
`.min(32)`-shaped result. So the "no-op for the only format with committed
coverage" argument in `av1tilerows.report.md:279-281` is *sound* and still
licenses a change that is wrong at 4:4:4 — the one format the identity argument
says nothing about. **The 4:2:0 identity argument cannot certify a change to a
subsampling-parameterised expression; only the measured 4:4:4 value can.**

### 3.3 A second wrong table, in this lane's own charter

The ground-truth table I put in the four scouts' briefs was reconstructed from
memory rather than measured, and **23 of its 66 rows were wrong** (15 real
errors + 8 `(shape, ss)` pairs that map to `BLOCK_INVALID`, which I had noted
but did not tabulate). Seven errors were in the **4:2:0** column — `8X8`,
`16X8`, `32X16`, `64X32`, `16X4`, `32X8`, `64X16` — and eight in the 4:2:2
column, which I had written as a verbatim copy of 4:4:4. The measured table in
§1.0 is authoritative. No verdict in this report was decided against the bad
table: every scout re-derived the rule from the cited functions, which is how
the discrepancy surfaced (they flagged the charter's table, not the code). The
concrete lesson: **a ground-truth table handed to a subagent must be produced
by running the reference, not by transcribing it.** The `av1tilerows` report
made the same class of error with `max_txsize_rect_lookup` — it read the table
correctly and misread the *function that consumes it*.

### 3.4 Rules confirmed, for the record

* `cu_tx == 32` at **every** subsampling; the 4:4:4 unit count is
  `((64 >> ss_x) / 32) × ((64 >> ss_y) / 32)` = **1 / 2 / 4** at 4:2:0 / 4:2:2 /
  4:4:4.
* A per-axis `.min(32)` on an **already-subsampled** chroma extent is an exact
  transcription of `av1_get_adjusted_tx_size` over the whole `TX_SIZE` domain.
* A per-axis `<< ss` on a chroma unit is the unit's **LUMA** footprint, not its
  transform size. The two must not be conflated — that conflation is the
  ticket's core error.
* At 4:2:0 the chroma transform is ≤ 32 on **both** axes for **every** block
  size, so a hardcoded 32 that is right at 4:2:0 proves nothing at 4:4:4.
* 4:2:2 is refused by name at the sequence header, so the 4:2:2 column — the
  only one where the per-axis walks are actually *distinguishable* — is
  unreachable in committed code. Every 4:2:2-only defect in this report
  therefore needs the env-gated probe to witness, and the probe must never be
  committed.

---

## 4. Invariants

All run in `/home/tahinli/.cache/wt/av1txsizeaudit` at `2fcc577a`
(main `9b2f6c9d` + the `4bfe8d8e` fix rebased), `CARGO_TARGET_DIR=$HOME/.cache/cargo-target`.

| gate | result |
|---|---|
| `a_real_aomenc_444_intra_in_inter_128_root_codes_chroma_per_mu_chunk_unit_pixel_exact` | **ok** — `3 decode-order frames pixel-exact (0 hidden), intra_128_in_inter=1 mu_chunk_chroma_reads=4` |
| `a_real_aomenc_inter_128x128_none_root_decodes_pixel_exact` | ok |
| `a_real_aomenc_128x128_none_inter_blocks_coded_chroma_per_mu_chunk_decodes_pixel_exact` | ok |
| `a_real_aomenc_lossless_444_key_frame_decodes_sample_exact` | ok |
| `a_lossless_444_min_partition64_inter_stream_decodes_pixel_exact` | ok |
| `a_lossless_444_128_root_lossless_stream_reads_chunks_chunk_major` | ok |
| `a_lossless_444_intrabc_rect_leaf_walks_per_4x4_units` | ok |
| `a_lossless_444_rect16x4_chroma_reach_is_ss_aware` | ok |
| `a_444_lossless_sb64_intrabc_rect_chroma_walks_4x4_units` | ok |
| `a_lossless_444_10bit_inter_stream_decodes_pixel_exact` | ok |
| `a_lossless_444_min_partition8_inter_stream_decodes_sample_exact` | ok |
| `a_lossless_444_defaultp_inter_strip_stream_decodes_byte_exact` | ok |
| `a_lossless_444_8bit_untiled_control_and_its_two_tile_column_sibling_decode_pixel_exact` | ok |

13 passed / 0 failed. The 4:2:0 identity (`>> ss == /2` there) and the new 4:4:4
gate both hold. No production file was edited by this lane; the tree is
`9b2f6c9d` + the replayed fix, so every row above is a statement about the
tree the fix will land on.

Artifacts: `~/.cache/txaudit/probe.c`, `~/.cache/txaudit/out.txt`,
`~/.cache/txaudit/table.md`, `~/.cache/txaudit/grad444.obu`,
`~/.cache/txaudit/coeffs.txt`, `~/.cache/txaudit/gate444.log`,
`~/.cache/txaudit/idset.log`.

---

## 5. Follow-up: C1 and C2 chased to a decision

This section answers the follow-up: C1 gets the witness it was chartered
without, the answer is **not** the one the charter assumed, and — after a
reconciliation with `Volkan-2`'s independent sweep of the same site — C1 is
**LANDED** with a red-before-proven source-scan gate. C2 gets its one-line
fix, a measurement plan, and a 50-site sweep of its class; C2 is **not**
landed, and the reason is given in §5.2.

### 5.0 Reconciliation with `Volkan-2` — was `fctx` in scope?

`Volkan-2` swept hardcoded chroma halvings independently, reached this same
function, and recorded it as **unfixable without threading `fctx` into
`Neighbours`**, because the method is a `Neighbours` method with no `fctx`
parameter and its single caller passes no format test. **That structural
finding is correct and I confirm it.** `Neighbours` (decode.rs:8444) has no
`fctx` field — its fields are the neighbour bands (`above`, `left`,
`above_mode`, `left_mode`, `above_uv_mode`, `left_uv_mode`, `above_side`,
`left_side`, `above_side_mi`, `left_side_mi`, …) and nothing that carries a
subsampling.

And to answer the question directly, because it changes what the measurement
in §5.1.2 was worth: **`fctx` was NOT in scope, and my first attempt did not
compile.**

```text
error[E0609]: no field `fctx` on type `&mut decode::Neighbours`
  --> (the line I had written as `ss_x(self.fctx)`)
```

I then threaded it — one parameter on the method signature, one argument at
the single call site — and it built clean. **So the `uv_w 64 -> 128`
measurement in §5.1.2 was a real change with real threading, not a temporary
hardcode and not a probe hack.** Had it been a hardcode, §5.1.2's zero-pixel
result would have been meaningless; it is not.

The minimal threading, stated as shipped:

| what | change |
|---|---|
| `suppress_internal_lf_edges` signature | `+ fctx: &FrameCtx` — one parameter |
| its single caller (was `43956`) | `+ fctx` — one argument |
| `struct Neighbours` | **unchanged** — no new field |
| call sites total | **1** |

Arithmetic first, then measurement, for the 4:2:0 no-op claim: `w_mi * MI` is
a multiple of `MI = 4`, hence even, hence at `ss_x == 1`
`(w_mi * MI) / 2 == (w_mi * MI) >> 1` for **every** reachable `w_mi`. Exact
identity, not an approximation. Then measured: 13/13 identity gates green on
the landed tree (§4 / §5.5), including the two 4:2:0 128-root gates
`a_real_aomenc_inter_128x128_none_root_decodes_pixel_exact` and
`a_real_aomenc_128x128_none_inter_blocks_coded_chroma_per_mu_chunk_decodes_pixel_exact`.

**Line numbers, as of the rebase onto `08f8ffb3`.** §1's tables were measured on
the audit base (`2fcc577a`) and §5's against the pre-rebase tree. The branch is
now rebased onto current main, and **every decode.rs reference in this report
carries the old base's numbering**. The mapping for the sites this report
cites most (decode.rs is 56 316 lines on the new base):

| site | old base | on `08f8ffb3` |
|---|---|---|
| `suppress_internal_lf_edges` (C1) | 9102 | **9277** |
| its caller (C1) | 43956 | **44297** |
| C2's `around_mi(unit_mi, unit_luma_w)` | 42974 | **43315** |
| `around_mi(lmi, B4)` (`decode_inter_sub8_split4`) | 44401 | **44742** |
| `around_mi(group_mi, 8)` (`decode_intra_sub8_leaf`) | 44913 | **45254** |
| `decode_intra_sub8_leaf` (fn) | 44999 | **45340** |
| `decode_intrabc_owned_rect`'s `bw / 2, bh / 2` (open cell) | 16558 | **16832** |
| `cfl_ac_q3_at`'s `/ 2` (non-site) | 19636 | **19920** |

Two independent causes, and they compose: the landed C1 change adds a 26-line
doc comment, and main's wave-3 merges added ~693 lines to decode.rs between
the audit base and `08f8ffb3`. `git diff 08f8ffb3 --stat` is **byte-identical
in shape** to `git diff 9b2f6c9d a5737713 --stat` — same 7 files, same
1511 insertions / 103 deletions — which is the signal that main's waves did not
collide with any of my hunks (§5.5).

The 50-site `around_mi` sweep in §5.3 is unaffected in substance: the
decode.rs diff touches **zero** gather call sites
(`git show HEAD -- crates/ | grep -cE '^[+-].*around_mi\('` = 0), so every
verdict stands and only the line numbers move.

### 5.1 C1 — the witness EXISTS, and it proves the defect is LATENT

`lanes/av1txsizeaudit.report.md` §2.2 chartered C1 with "no fixture exists".
**That was wrong, and the correction matters more than the charter.** The
arm is reachable, the recipe is trivial, and the guard fires on the first
try. The pixels still match libaom — and that is not a failure to find the
witness, it is a proof that no witness can exist.

#### 5.1.1 The recipe (2 commands, no probe, no knob hunting)

```sh
# a STATIC 128x256 4:4:4 y4m. Static is the operative ingredient: identical
# frames give zero MV and zero residual, so the RD picks a whole-block skip.
# Moving content never reaches the arm.
python3 -c "
w,h,n=128,256,4
Y=[[((x//8+y//8)*60+30)%256 for x in range(w)] for y in range(h)]
U=[[((x//16)*60+30)%256 for x in range(w)] for y in range(h)]
V=[[((y//16)*60+30)%256 for x in range(w)] for y in range(h)]
f=open('static.y4m','wb'); f.write(b'YUV4MPEG2 W%d H%d F25:1 Ip A1:1 C444\n'%(w,h))
for k in range(n):
    f.write(b'FRAME\n')
    for P in (Y,U,V): f.write(bytes(v for row in P for v in row))
f.close()"

~/.cache/aom-oracle/build/aomenc --codec=av1 --profile=1 --sb-size=128 \
  --limit=4 --cq-level=60 -o c1_static.obu static.y4m
```

`--profile=1` (4:4:4) and `--sb-size=128` are the two load-bearing knobs.
`--cq-level` is **not** load-bearing — it produced byte-identical streams at
20/40/60, because the residual is already zero. The SB is 128x128 so a
`--limit=4` encode gives 2 roots per frame.

| stream | sha256 | guard fires | frames |
|---|---|---|---|
| `c1_static.obu` | `363672a2db670e08d70ee9e6c4ab40d8fc1f6b1bb1276cd5d88bbcb6925f57c8` | 6 | 4 |
| `c1_step.obu` (hard vertical step at x=64) | `07b2783811d1ea209dacc18f06bb166cbfe635d3c7c40ec5d967795c9d2cd9d8` | 6 | 4 |
| `bars_20.obu` (16-px bars, static) | `1c5b144c47eb5b858c722e87b4ed2d69a470ad94b8b8f507452736b0bac9fca0` | 10 | 6 |
| `grad6_20.obu` (moving gradient) | `5facd9355482cada137870e5c94c7410b97bdd27ca96c436afa13371ac9d5bd0` | 2 | 6 |
| `noise444_20.obu` (static noise) | `c9c1b48205da19d12a061c44a04f90baefc91362b28667c996d7064415b75ed7` | 6 | 6 |

**30 guard fires across 26 frames / 2 555 904 samples**, all at ss (0, 0), all
`w_mi = h_mi = 32` (a 128x128 luma root). The oracle agrees independently —
`aomdec`'s own `AOMMB` rung prints `mi=(0,0) mode=13 rf=(1,-1) mv0=(0,0)
mv1=(0,0) skip=1`: skip mode, compound, zero MV, `skip=1`.

#### 5.1.2 The red-before that cannot exist

The fix is one expression, and it is the sibling's exact form:

```rust
-        let (uv_w, uv_h) = ((w_mi * MI / 2).max(4) as u8, (h_mi * MI / 2).max(4) as u8);
+        let (uv_w, uv_h) = (
+            ((w_mi * MI) >> ss_x(fctx)).max(4) as u8,
+            ((h_mi * MI) >> ss_y(fctx)).max(4) as u8,
+        );
```

(this also threads `fctx` into `suppress_internal_lf_edges` and its one
caller at `43956`.) Measured, on the corpus above:

| build | published extent at 4:4:4 | vs oracle | vs the other build |
|---|---|---|---|
| base (`/2`) | `uv_w=64 uv_h=64` | **0** differing of 2 555 904 | — |
| fixed (`>> ss`) | `uv_w=128 uv_h=128` | **0** differing of 2 555 904 | **0** differing |

**The fix changes the published value from 64 to 128 and changes not one
sample.** So there is no red-before to produce, on this corpus or any other.

#### 5.1.3 Why — the masking mechanism, named

The widening exists to defeat the deblocker's alignment gate at
`decode.rs:29514`:

```rust
if cur_tx == 0 || (if c.dir == 0 { x0 } else { y0 }) & (cur_tx as usize - 1) != 0 {
    return None;
}
```

With `cur_tx = 64` a chroma edge at `x0 = 64` survives (`64 & 63 == 0`); with
`cur_tx = 128` it dies (`64 & 127 = 64`). So the widening *does* do what it
was written to do. But the survivor then meets, 68 lines later, the **same
libaom rule applied at the correct place** — `decode.rs:29582-29589`:

```rust
// spec 7.14.2 / libaom `set_lpf_parameters`: a transform edge between two
// SKIPPED INTER blocks is filtered only when it is also a prediction
// (coded-block) edge ...
if !pu_edge
    && cur_ref != 0
    && pv_ref != 0
    && n.skip_at(mi_r, mi_c)
    && n.skip_at(pv_mi_r, pv_mi_c)
{
    return None;
}
```

Inside a single skipped 128 root, **every** internal edge satisfies all four
terms: `pu_edge` is false because the chroma `plane_dim` is
`((32 * 4) >> 0) = 128` and `64 % 128 != 0` (`decode.rs:29577-29578`), and
both `skip_at` are true and both refs nonzero because both cells are the same
compound skip-mode block. At the block's own boundary (`x0 = 128`) both the
buggy and the fixed value pass the alignment gate identically, and there the
edge *is* a `pu_edge`, so both builds filter it the same way. Hence zero
observable difference, for every stream.

**This is total, not incidental.** `suppress_internal_lf_edges` is only ever
called for a block that is itself skipped (`43954`: `skip_inter_128 = skip &&
is_inter && write_w.max(write_h) > 64`), so there is no input on which one
side of an internal edge is not skipped. The defect cannot be unmasked by any
fixture, at any `--cq-level`, on any content.

#### 5.1.4 Disposition — C1 LANDED, for spec fidelity, with no observable effect

The original charter said "no fixture exists". Both halves of that were wrong
in different ways, and the corrections matter more than the charter:

* **The fixture exists** (§5.1.1): two commands, arm fires on the first try,
  30 fires over 26 frames across five streams. `Volkan-2`'s independent rung
  (5 hits over the 444 gate set) reached the same reachability conclusion by a
  different route; the two agree.
* **C1 is a real mismatch against libaom.** `get_plane_block_size(BLOCK_128X128,
  0, 0)` is `BLOCK_128X128` (`common_data.c:38`), so a 128 root's own chroma
  extent at 4:4:4 is **128**, not the 64 the `/2` published. Measured, not
  argued (§1.0, §0.2).
* **C1 is provably unobservable**, by the mechanism in §5.1.3 and the 26-frame
  measurement in §5.1.2 — and **total, not incidental**, because the method
  is only ever called for a block that is itself skipped.
* **LANDED**, in this lane, on Main's authority, with the disposition sentence
  that was missing: **fix for spec fidelity, no observable effect on any
  current format.** The masking mechanism is recorded in the method's own doc
  comment so no future reader hunts for a pixel regression that cannot exist.
* **And one of my own claims is retracted here.** My first version of this
  disposition said "*no gate can be written for it*" because no *pixel* gate
  can exist. That was overreaching: a **source-scan gate can**, and it is
  landed and **red-before proven** (§5.1.5). The precise claim is: *no pixel
  gate can exist; the source scan is the only instrument, and it works.*
* **What is NOT an unblock condition:** fixture engineering. The recipe is
  above; it works. The old blocker was structural, and it has been discharged.

#### 5.1.5 The gate, and the red-before

New gate
`refusal_inventory::tests::the_skipped_128_root_chroma_suppression_publishes_the_blocks_own_per_axis_chroma_extent`
(`refusal_inventory.rs:1765-1829`). It reads `suppress_internal_lf_edges`'s own
body out of `decode.rs` — the same class-and-reader-move-together instrument
this file already uses at `1573` and `1577` — and asserts three things:

1. the body contains `((w_mi * MI) >> ss_x(fctx))` and
   `((h_mi * MI) >> ss_y(fctx))`;
2. the body contains **neither** `w_mi * MI / 2` nor `h_mi * MI / 2`;
3. the signature carries `fctx: &FrameCtx` — pinning the threading itself, so
   a future edit that drops the parameter (and with it the only route to the
   subsampling) fails loudly instead of reintroducing a literal.

**Red-before, produced by reverting ONLY the arithmetic** (the gate, the
threaded parameter, the caller argument and the doc comment all kept):

```text
line 9130 -> let (uv_w, uv_h) = ((w_mi * MI / 2).max(4) as u8, (h_mi * MI / 2).max(4) as u8);

thread '...the_skipped_128_root_chroma_suppression...' panicked at
  crates/ec-av1/src/refusal_inventory.rs:1807:9:
suppress_internal_lf_edges must publish the block's own chroma extent per axis
-- `w_mi * MI >> ss_x(fctx)`, not a hardcoded `/2`
test result: FAILED. 0 passed; 1 failed
```

Restored -> `ok`. **This is the red-before §5.1.2 said could not exist.** It
cannot exist on *pixels*; it exists on the source scan, which is the correct
instrument for a value that is provably correct and provably unobservable.

Identity set on the landed tree: **13 passed / 0 failed** (§5.5).

### 5.2 C2 — the one-line fix, and why it is not landed

`decode.rs:42974`:

```rust
-                                let cu_around = neighbours.around_mi(unit_mi, unit_luma_w);
+                                let cu_around =
+                                    neighbours.around_mi_rect(unit_mi, unit_luma_w, unit_luma_h);
```

`unit_luma_w`/`unit_luma_h` are the per-axis pair computed at `42907-42908`;
the arm's own doc comment at `42904-42906` even says the size is "the size
`around_mi` / `record_mi_chroma` want", and `record_mi_chroma` at `42996` is
per axis — so `around_mi` at `42974` is the leftover of the same collapse the
lane's fix repaired everywhere else in the arm. The inter twin is
`14191-14192`, with the same pair at `40212` and `41999`.

**Measured:** applied and reverted. Five gates re-run on the patched tree
(`a_real_aomenc_444_intra_in_inter_128_root_...`, `..._inter_128x128_none_root_...`,
`..._128x128_none_inter_blocks_coded_chroma_per_mu_chunk_...`,
`a_lossless_444_10bit_inter_...`, `a_lossless_444_rect16x4_chroma_reach_is_ss_aware`)
— **5/5 ok**, byte-identical, exactly as §5.3's reachability predicts.

**Not landed**, per instruction and on the merits: the arm is `side > 64`
(`42871`) and 4:2:2 is refused by name at the sequence header
(`stream.rs:1749-1754`, gate `a_non_420_subsampled_sequence_header_is_refused_by_name`),
so there is no committed stream — and no committable one — on which this line
can be red. A change that cannot be made red cannot be gated, and an ungated
change to a 4:2:2 path is the exact hazard `lanes/av1ibc128arm.report.md`'s
`EC_AV1_ALLOW_422_PROBE` note exists to prevent.

**Measurement plan for whenever 4:2:2 is unblocked** (a 4:2:2 lift lane, probe
never committed):

1. Encode `aomenc --codec=av1 --profile=2 --sb-size=128 --limit=4` over a
   static 128x256 `C422` y4m (profile 2 = 4:2:2), static for the same
   reason as C1: zero MV + zero residual keeps the 128 root on the arm.
2. Decode with `EC_AV1_ALLOW_422_PROBE=1` and `EC_DCDUMP=1`. `around_mi_rect`
   already dumps `EC_DCDUMP ... wh=(w,h) above=[..] left=[..]` (`9707-9711`);
   the square `around_mi` does not dump at all, which is itself the
   observable: at `ss (1,0)` the fixed build prints `wh=(64,32)` and a left
   band of 8 mi, the unfixed build a left band of 16.
3. Red-before: revert the one line, re-run, the `EC_DCDUMP` left band doubles
   and `dc_sign_ctx` diverges on the first chroma unit of the 128 root — an
   arithmetic-coder fork, visible as an `EC_COEFF_STEP ... dcctx=` mismatch
   against the oracle's own `EC_TRACE_COEFF`.
4. Green: re-apply, the fork is gone. The 4:2:0 and 4:4:4 identity sets must
   not move (they provably cannot; `unit_luma_w == unit_luma_h` at both).

### 5.3 The C2 class sweep — 50 `around_mi(` sites, 1 instance, 0 new

The question the class asks: *is the size passed to the square gather provably
equal to the height libaom would use at that same point?* All 50 sites were
resolved by reading each one's enclosing guard. **49 correct, 1 instance (the
known `42974`), 0 unresolved.** Critically, **no luma rect-transform leaf
passes one axis unguarded** — so there is no 4:2:0-reachable instance, which
is what would have made this a live defect rather than a 4:2:2 lift-blocker.

| pattern | sites | why correct |
|---|---|---|
| explicit `if tw == th` guard | 13640, 16691, 17240, 40057, 41838, 46336 | each has its `around_mi_rect(tu_mi, tw, th)` sibling in the same `else` (13661, 16711, 17260, 40088, 41869, 46363) |
| precomputed `rect_unit` flag | 17740 | this is the `else` of `rect_unit = tx_w != tx_h`; twin 17689 |
| `(tw, th) == (4, 4)` guard | 25748 | twin 25759 |
| structurally square, no assert | 14089, 21463, 42857, 49421 | the leaf list is square by construction (a square tree root + `sub_tx_size_map`, which maps only 1:4 shapes to rect; or `read_block_tx_size`'s `is_inter=false` path pushing `(row, col, tx, tx)` from a scalar `read_tx_size`), **and** the coefficient reader is square-only, so a rect leaf would break the read before the gather mattered. 14089 documents the invariant with `debug_assert_eq!`; the other three rest on the construction argument alone — see the caveat below |
| `!rect_tu` block guard | 40008, 40014, 41726, 41732, 43169 | on the non-rect arm, where the block's luma plane is provably `side × side`; the 4:2:2 case is rerouted to `around_mi_422_chroma` or a `chroma_w == chroma_h` test |
| sub-8x8 4:2:0 group tails | 25300, 25353, 26478 | under an explicit `else` of the `chroma_444` / `chroma_422` arms |
| provably square by arm analysis | 11848 | the final `else` of a four-arm `skip` / `luma_64corner` / `Some(luma_rect)` chain: `luma_rect == None ∧ ¬luma_64corner` implies `tx_w == tx_h`; the read at 11867-11868 also passes `tx_w, tx_w` |
| scalar literals / provably-square scalars | 17214, 18057, 22180, 22962, 23135, 23234, 24961, 25080, 26011, 29066, 29095, 36097, 36275, 40463, 42256, 43273, 44401, 44517, 45203, 45255, 45629, 46920, 47955, 48912, 49508 | `4`, `8`, `MI`, `B4`, or a `chroma_tx`/`logical_tx`/`cu_tx` scalar on an `ss_x == 0 && ss_y == 0` arm. `23234`'s `8` covers 4:2:0 (chroma 4x4 → luma span 8x8 = 2x2 mi) and 4:4:4 lossy (chroma 8x8 → luma 8x8 = 2x2 mi), and 4:2:2 is intercepted at 23242 |
| **INSTANCE** | **42974** | **no `unit_luma_w == unit_luma_h` guard; differs at 4:2:2 only; correct twin `14192`/`40212`/`41999`** |

The multi-line call at `45744` — `around_mi(if chroma_444 { lmi } else { group_mi },
if chroma_444 { 4 } else { 8 })` — is **not** an instance: both arms are
scalars, and it sits in the `else` of `chroma_444 && bw != bh` (`45603`),
whose own correct per-axis twin is `around_mi_rect(lmi, bw, bh)` at `45691`.

**Caveat worth carrying forward (low value today, a trap tomorrow).** Four
sites — `21463`, `42857`, `49421`, `14089` — consume a var-tx or intra leaf
list while **discarding the leaf's height field** (`for &(lr, lc, tw, _th) in
leaves`). They are correct only because the reader is also square-only and
the list is square by construction. If a future change lets any of these arms
see a rect leaf, the gather AND the reader must be split to
`around_mi_rect` in the same step, or the first rect leaf that arrives forks
the coder.

### 5.4 One adjacent finding, not in the C2 class

`decode_intra_sub8_leaf` (44999-45827) has **no `chroma_422` branch at all** —
only `chroma_444` (`45488`). At 4:2:2 its 8x8 group's chroma plane block is
4x8, and the gather at `45744` would need `around_mi_422_chroma`'s
every-second-above-cell resampling (`9594`), not the plain per-mi sum. This is
a **missing-422-guard** finding, not a per-axis one, so it is outside C2's
class; it is unreachable while 4:2:2 is refused, and it belongs to the 4:2:2
lift as a fourth item beside C2. Every sibling 8x8 path does carry the 422
branch: `decode_leaf8` (`23231`), `decode_inter_block8` (`47962`/`48919`),
`decode_inter_sub8_rect2` (`44811`, `46809`).

### 5.5 State of the tree — rebased onto `08f8ffb3`

**C1 is landed; C2 is not.** The C2 one-liner and its `EC_C1_PROBE`
scaffolding were reverted with `git checkout` after measuring, exactly as
§5.2 states.

This branch was rebased onto current main (`08f8ffb3`) and **one near-miss is
worth recording**, because it is the same trap as the stale-base diff, one
level deeper. The first rebase attempt saved a patch of
`2fcc577a..HEAD` — that range contains only *my* r2/r3 work, because
`2fcc577a` is itself the fix commit replayed on the old main. Applying it to
`08f8ffb3` produced a tree that built, passed `cargo check --all-targets`, and
passed 12 of 13 identity gates — because the intra-in-inter fix and its
`a_real_aomenc_444_intra_in_inter_128_root_codes_chroma_per_mu_chunk_unit_pixel_exact`
gate were **silently absent**: they live in commit `4bfe8d8e`, which is on this
branch and has never been on main. The tell was a green run in which one
named test produced no output line at all. The fix was to diff the whole
branch against the old main, `git diff 9b2f6c9d a5737713`, since
`9b2f6c9d` is an ancestor of `08f8ffb3`; that patch restores all 7 files.

Scope, checked by shape rather than by the apply's clean status:

```text
git diff 08f8ffb3 --stat
 crates/ec-av1/examples/decode_probe.rs |   5 +
 crates/ec-av1/src/decode.rs            | 305 +++++++++----
 crates/ec-av1/src/refusal_inventory.rs |  66 +++
 crates/ec-av1/src/stream.rs            | 192 ++++++++
 lanes/av1ibc128arm.report.md           | 196 ++++++++++++
 lanes/av1tilerows.report.md             |  38 +-
 lanes/av1txsizeaudit.report.md          | 812 +++++++++++++++++++++++++++++++++
 7 files changed, 1511 insertions(+), 103 deletions(-)
```

**Byte-identical in shape to `git diff 9b2f6c9d a5737713 --stat`** — same 7
files, same 1511/103 — which is the signal that main's wave-3 merges
(+693 lines to decode.rs alone) did not collide with any of my hunks.

Silent-restore check, the one Main flagged: for each source file, the set of
lines main **deleted** between `9b2f6c9d` and `08f8ffb3` was intersected with
the set of lines my tree **adds**, restricted to distinctive lines
(`length >= 45`, so `} else {` and `assert!(` do not generate noise):

```text
crates/ec-av1/src/decode.rs          : 0
crates/ec-av1/src/stream.rs          : 0
crates/ec-av1/src/refusal_inventory.rs : 0
```

**Zero.** Nothing another lane removed has been restored. In `decode.rs` the
only removed lines are the three I meant to change plus the one stale doc line
whose "chroma extent (64)" claim is exactly what C1 corrects.

Re-run on the rebased tree, after `touch`ing both touched sources:

```text
cargo check -p ec-av1 --all-targets        Finished, clean
the new source-scan gate                   1 passed / 0 failed
gate_coverage::                            13 passed / 0 failed
refusal_inventory::                        20 passed / 0 failed
the 13-gate identity set                   13 passed / 0 failed
```

47 tests, 0 failures, including `a_real_aomenc_444_intra_in_inter_128_root_codes_chroma_per_mu_chunk_unit_pixel_exact`
— present and green, which is the check the first rebase attempt silently
failed to run. `~/.cache/txaudit/r4.log`.

New artifacts: `~/.cache/txaudit/c1/{static,step,bars,grad6,noise444}.y4m`,
`~/.cache/txaudit/c1/*.obu`, `~/.cache/txaudit/c1/sweep.sh`,
`~/.cache/txaudit/full_branch.patch`, `~/.cache/txaudit/r4.log`.

#### 5.5.1 PROVENANCE — this branch is not only C1

Declared, because it is better found here than at merge time.

**`lanes/av1txsizeaudit` carries TWO lanes' work, not one.**

| what | produced by | on main? | declared here |
|---|---|---|---|
| the tx-size audit, the C1 fix + gate, the `av1tilerows` retraction | **`lane-av1txsizeaudit`** (this lane; commits `1dbe80d3`, `3b44339c`, `a5737713`, `dc80b6fa`, plus the rebase commits) | no | §1-§5 of this report |
| the 4:4:4 intra-in-inter 128-root per-unit chroma walk (279 lines of `decode.rs`, 192 of `stream.rs`, 5 of `examples/decode_probe.rs`, 196 of `lanes/av1ibc128arm.report.md`) | **`lane-av1ibc128arm`**, commit **`4bfe8d8e`** "av1: 4:4:4 intra-in-inter 128-root mu-chunk chroma as a per-unit walk" | **NEVER MERGED — confirmed** | this table |

The three checks that establish it, run against `main` directly:

```text
git merge-base --is-ancestor 4bfe8d8e main          -> NO
git grep -c 'a_real_aomenc_444_intra_in_inter_128_root_codes_chroma_per_mu_chunk' main
    -- crates/ec-av1/src/stream.rs                  -> 0
git grep -n 'luma_span = cu_tx * 2' main -- crates/ec-av1/src/decode.rs
                                                  -> main:…:43185
```

That last line is the proof of *non*-merge rather than a re-implementation:
**main still carries the collapsed 4:2:0 arm verbatim** — `let luma_span =
cu_tx * 2;` at `main:crates/ec-av1/src/decode.rs:43185` — which is exactly the
line `4bfe8d8e` deletes. The same string appears 0 times on this branch. So
`4bfe8d8e` is not a duplicate of something already landed; it is unmerged work
riding here, and it is the only copy.

* **Has it been merged under another name?** No. `git log --all -S"luma_span =
  cu_tx * 2"` finds only the two lanes that *created* the class
  (`2050a065` lane-av1-ibc128chunk, `29550383` lane-av1-422bigblock) and the
  commit that removed it (`4bfe8d8e`). Nothing re-landed it.
* **Does another lane's report claim it?** No. `git grep -ln
  "intra_in_inter_128_root" main -- lanes/` returns nothing;
  `lanes/av1ibc128arm.report.md` does not exist on main at all
  (`git cat-file -e main:lanes/av1ibc128arm.report.md` fails). It exists only
  on the unmerged `lane-av1ibc128arm`, whose tip is `4bfe8d8e`.
* **Branch `lane-av1ibc128arm` still exists**, tip `4bfe8d8e`, so this is not an
  orphan whose only home is here.

**The two foreign report files, declared individually:**

* `lanes/av1ibc128arm.report.md` (+196) — **deliberately kept, and it belongs
  to `lane-av1ibc128arm`, not to this lane.** It is the only provenance record
  for the 279-line `decode.rs` change and the 4:4:4 gate, and a report that
  does not ship with its code is exactly how evidence is lost. It is not
  cherry-pick residue: it arrived with `4bfe8d8e`, whole, as part of that
  commit. **Merge owner:** if `lane-av1ibc128arm` is also going to be merged,
  take the code and this report from whichever branch lands first and drop the
  duplicate; do not let both branches add the same path. If it is not going
  to be merged separately, this branch must carry it — there is no other copy.
* `lanes/av1tilerows.report.md` (38 changed) — **mine, and deliberately
  so.** It is the retraction of the `cu_tx = 32 << ss_x` rule at that report's
 own origin (`lanes/av1tilerows.report.md:265-281`, the §8 "Recommendation"),
 which is the one committed instance of the refuted rule in the repository.
  Zero code, zero gate; a `RETRACTED by lanes/av1txsizeaudit.report.md §3.1`
  blockquote plus the correction and the oracle measurement. It must land
  whenever this branch lands, or the rule stays re-derivable.

**Contention to hand to the merge owner, NOT pre-resolved here:**
`decode_intrabc_owned_rect`'s `let (cw, ch) = (bw / 2, bh / 2);` — the open
cell this report lists at §2.3 and §5.0 (line 16558 on the audit base, **16832
on the current base**) — is being fixed by another lane right now. This
branch does **not** touch that line: `git diff ec4e9528 -- crates/ec-av1/src/decode.rs`
contains no hunk near it, and it appears in this report only as a *finding*.
Expect the region to contend at merge and resolve it there; do not pre-empt it
from here, because the other lane holds the witness and this one holds only
the observation.

#### 5.5.2 Re-rebase onto `ec4e9528` — a second patch-base trap, same family

`main` advanced from `08f8ffb3` to `ec4e9528` (one append to
`lanes/av1merge-wave3d.report.md`). The re-rebase repeated r4's error in the
opposite direction, and it is worth recording because the two together are the
whole rule:

* r4's near-miss: patch from **"my base"** `2fcc577a` — which is itself the
  unmerged fix commit — and silently drop that commit's work.
* r5's near-miss: patch from **"the old main"** `9b2f6c9d` — but by then this
  branch's tip sat on the *new* chain, so `git diff 9b2f6c9d HEAD` swept in
  **all** of main's wave-3 changes: 18 041 patch lines touching `crates/ec-aac`
  and binary fixtures, which failed to apply and left an empty diff.

**The rule both times: the patch's left-hand side must be the commit this
branch's base actually is (`git merge-base HEAD main`), never a remembered
ancestor and never a self-descriptive "my base".** Verify with
`git apply --stat` BEFORE applying — the wrong base is obvious there (it names
files you have never touched) and is invisible in `--3way`'s exit status.

Correct patch, `git diff 08f8ffb3 99f25b1f`, 1 894 lines, 7 files. After
`git apply --3way` onto `ec4e9528`:

```text
git diff ec4e9528 --shortstat
 7 files changed, 1679 insertions(+), 103 deletions(-)
```

**Identical to the pre-rebase shape**, and the removed-line counts are
identical per file (decode.rs 97, av1tilerows 6, all others 0). Main's move
touched **nothing** under `crates/ec-av1/`
(`git diff 08f8ffb3 ec4e9528 --stat -- crates/ec-av1/` is empty), so
`decode.rs` is 56 316 lines with `suppress_internal_lf_edges` at 9277 and C2 at
43315 — **§5.0's line-shift table is valid verbatim on this base, with no
adjustment.** Silent-restore check against this move: 0/0/0.

Re-run after `touch`ing both touched sources:

```text
cargo check -p ec-av1 --all-targets        Finished, clean
the new source-scan gate                   1 passed / 0 failed
gate_coverage::                            13 passed / 0 failed
refusal_inventory::                        20 passed / 0 failed
the 13-gate identity set                   13 passed / 0 failed
```

47 tests, 0 failures, 4:4:4 gate present and green. `~/.cache/txaudit/r5.log`.
