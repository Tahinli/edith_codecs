# lane-av1422ctintrabc — the 4:2:2 intrabc chroma COPY geometry is SOUND on the whole corpus; the defect was arm ORDER, already fixed at my tip; one named latent gap (4x8 leaf extent, reach 0/12) is handed over, not patched

**Outcome in one line: I reproduced the four cells independently (sha, first bad
byte, seed 182-vs-202), re-derived the libaom copy geometry expression by
expression, measured all four cells plus the reproducer plus both controls to
byte-exact on every plane of every decode frame, showed the flip control bites on
both states, showed all five intrabc-heavy regression witnesses hold on BOTH
states, and refuted all four named suspects — with one measured latent gap handed
over rather than patched.**

Tip: `main` = `1686dc8a` (the merge of `lane-av1422ctattr`). Worktree
`/home/tahinli/.cache/wt/av1422ctintrabc`, branch `lane-av1422ctintrabc`.
**Zero commits, zero source changes** — `git status --porcelain` in the lane tree
is empty, and in the primary checkout it is empty. The 4:2:2 sequence-header
bypass was applied only to *build a measurement binary* and is **restored**
(`stream.rs:1803` reads `if seq.subsampling_x != seq.subsampling_y {`). No push,
no merge, no rustfmt. The shared oracle binary was **not** overwritten; a private
oracle was built at `/home/tahinli/.cache/aom-oracle-av1422ctintrabc`.

---

## 0. What the brief assumed, and what measurement says

| brief's premise | measured verdict |
|---|---|
| the copy geometry (source rect / per-axis DV scale / anchor rounding / extent) is the defect | **REFUTED** — every term matches libaom's expression term-for-term on all 12 reached calls (§3, §4) |
| there is something to fix in the copy geometry | **REFUTED for the corpus** — the defect was arm ORDER (`if skip` before the intrabc copy), merged at `1686dc8a`; I measured its removal to reproduce all four cells and its restoration to clear them (§2, §5) |
| a residual defect remains | **NOT FOUND on the corpus.** One latent gap is named with a reach census: a `BLOCK_4X8` leaf's chroma copy extent (§7) — **0 hits on 8 cells**, left unpatched on purpose |
| the fix must be gated | **NO.** 4:2:2 is refused at the sequence header; §8 states this plainly and does not substitute a source scan |

**Important correction to the prior lane's own record:** `lanes/av1422ctrigger`
§5c states our `lmi.1` is `2 * mi_col` and therefore "always even", and builds its
`EC_422_ANCHOR` floor probe on `(lmi.1 / 2) % 2 == 1`. **That is a unit error.**
`crates/ec-av1/src/decode.rs:25945` computes `let (px, py) = (lmi.1 * MI, lmi.0 * MI)`
with `MI = 4`, and `lmi` is the libaom `MI_SIZE`-4 grid, so **`lmi.1 == mi_col`**.
My probe prints the seed as `mi=(40,58) leaf=(8,4) cpx=116`, and
`cpx = (58 * 4) >> 1 = 116` — so `lmi.1` is `58`, libaom's own `mi_col`, not
`2 * 58`. The floor `& !1` therefore *does* fire on odd columns, and that lane's
probe tested the wrong predicate. Its "refuted" verdict on the anchor survives (I
refute it independently, §4b), but its stated reason does not.

---

## 1. Reproduction, from scratch, by me

Source (hand-built y4m; ffmpeg's muxer will not write a `C422` tag):

```
ffmpeg -v error -f lavfi -i testsrc2=size=320x242:rate=25 -frames:v 16 \
       -pix_fmt yuv422p -f rawvideo src/raw -y
# header "YUV4MPEG2 W320 H242 F25:1 Ip A1:1 C422\n", then "FRAME\n" + 320*242
# + 2*(160*242) bytes per frame
```

My y4m is sha-identical to the predecessor's, and so is the encode:

| artifact | bytes | sha256 |
|---|---|---|
| `src/testsrc2_320x242.y4m` (mine) | 2 478 215 | `3fa8891bde6f5345…` (first 16; identical to the predecessor's) |
| **`R422_320x242.obu` (mine, fresh)** | **20 733** | **`909e58db18fd4b7590501e2cecfbc090cde49eb26c9a42e15b1ef1fc265843d8`** |

The recipe is byte-deterministic: my fresh `aomenc` run reproduces the pinned hash
exactly, so the reproducer needs no archaeology.

### The seed, re-measured on my own BEFORE build

`Q_odd320x242.obu`, decode frame 0, `FINAL` dumps, chroma U:

| | value |
|---|---|
| above row y=159 x=116..119 | `202 202 202 202` — **byte-identical both sides** |
| left column x=115 y=160..163 | `165 164 163 152` — **byte-identical both sides** |
| **ours** | **182** |
| **oracle** | **202** |

Reproduced independently of either prior report. First bad decode frame / byte,
measured with `cmp` on `EC_AV1_FINAL_DUMP` decode-order dumps:

| cell | decode frames | first bad decode frame | first bad byte |
|---|---|---|---|
| `Q_odd320x242` | 17 | **0** | **103156** |
| `R422_320x242` (my fresh build) | 17 | **0** | **103156** |
| `O_odd322x242` | 17 | 0 | 106388 |
| `AB_inter_warp_odd` | 43 | 0 | 110876 |
| `S_odd326x242_10b` | 17 | 0 | 214112 |
| `R_odd322x240` (control) | 17 | — | — |
| `V_tile2x2_odd` (control) | 17 | — | — |

### Oracle control

The shared `~/.cache/aom-oracle/build/aomdec` is a **stale build**: it carries
`EC_DV`'s format string but fires **0** lines and contains neither `EC_DP` nor
`EC_PIB`, so the attribution lane's oracle evidence cannot be re-run against it.
I built a **private** oracle at `/home/tahinli/.cache/aom-oracle-av1422ctintrabc`
(`rm -rf build; cmake -S src -B build -DENABLE_EXAMPLES=1; make -C build -j16
aomdec` — the tree's generator emits Unix Makefiles, **not** ninja, despite its
own closing hint line saying `ninja`). Its `--rawvideo` output is
**byte-identical to the shared oracle binary's** on `Q_odd320x242`,
`R422_320x242` **and** `AB_inter_warp_odd`; every number in this report is a real
oracle diff, not a rebuild artefact. `EC_DP=1 EC_PIB=1 EC_DV=1` on the private
oracle emits 4 837 rung lines on `Q_odd320x242`, so rungs 19/20/21 are live.

### Disclosure: I wrote rungs into the SHARED oracle SOURCE tree

**`scripts/instrument-aom-oracle.sh` does not take a positional argument.** Its
target is `SRC="${AOM_ORACLE_SRC:-$HOME/.cache/aom-oracle/src}"`
(`instrument-aom-oracle.sh:81`). I invoked it as
`bash scripts/instrument-aom-oracle.sh /home/tahinli/.cache/aom-oracle-av1422ctintrabc/src`,
which silently **ignored my path** and instrumented the **shared** tree. That run
reported `EC_PIB/EC_DCIN instrumented` and `EC_DP instrumented` — i.e. it added
rungs 19/20/21 to `~/.cache/aom-oracle/src/av1/common/reconintra.c` and
`~/.cache/aom-oracle/src/av1/decoder/decodeframe.c` at 15:14.

**Blast radius, measured:**

* The shared oracle **binary is untouched** — `~/.cache/aom-oracle/build/aomdec`
  still has mtime `2026-09-30T10:36`, so no live lane running the shared binary is
  affected. A rung only takes effect on the next rebuild.
* The shared **source** is now in the state the repo's own tooling demands:
  re-running the generator against it is a full no-op, and
  `scripts/check-aom-oracle-rungs.sh /home/tahinli/.cache/aom-oracle/src`
  **exits 0** with every assertion green, including the three new ones
  (`rung 19 EC_PIB install sites … 1`, `rung 20 EC_DCIN install sites … 2`,
  `rung 21 EC_DP install sites … 1`). So no corruption and no divergence between
  tree and generator — the write completed a state that was pending anyway.
* I then re-synced my private oracle from the now-instrumented tree, rebuilt, and
  re-ran **every** measurement in this report on it: the eight-cell census and the
  five regression witnesses (§5, §6) are the numbers from that final binary.

---

## 2. THE NAMED EXPRESSION — what actually differed

**It was not the copy geometry. It was which arm ran.**

**libaom**, `av1/decoder/decodeframe.c:1000` and `:1040-1043`:

```c
  if (!is_inter_block(mbmi)) {
    …                                              /* intra: predict_and_recon_intra_block_visit */
  } else {
    td->predict_inter_block_visit(cm, dcb, bsize); /* <-- UNCONDITIONAL */
    // Reconstruction
    if (!mbmi->skip_txfm) {                        /* <-- skip gates ONLY the residual read */
```

and `av1/common/blockd.h` `is_inter_block`:

```c
static inline int is_inter_block(const MB_MODE_INFO *mbmi) {
  return is_intrabc_block(mbmi) || mbmi->ref_frame[0] > INTRA_FRAME;
}
```

so an INTRABC block takes the `else` arm and **predicts regardless of
`skip_txfm`**.

**Ours**, `crates/ec-av1/src/decode.rs`, `sub8_leaf_chroma422` — before the fix the
arms read `if skip { push_intra(DC_PRED) } else if let Some(dv) = { frame copy }`,
so a leaf that is **both** intrabc **and** `skip` predicted intra DC and the frame
copy never ran. That is our line, at `decode.rs:26118` (`} else if skip {`) in the
current file.

**Why the signature is "chroma-only, ~0.5 % of the plane, luma exact":**

1. **Luma exact.** The luma arm for the same leaf was already ordered correctly, so
   luma is byte-exact on every frame of all four cells — measured, not assumed
   (§5: `Y=0` everywhere).
2. **Chroma only.** `dec_build_inter_predictor` loops
   `for (plane = 0; plane < num_planes; ++plane) { if (plane && !xd->is_chroma_ref) break; … }`
   (`decodeframe.c:702`): the intrabc copy runs **only on chroma planes**, and only
   for the plane block the leaf owns. No other plane, no other region is touched.
3. **~0.5 % of the plane.** The unit is one TX_4x4 chroma unit, i.e.
   `4*4 = 16` samples out of `160*242 = 38 720` per chroma plane per frame — and
   only the intrabc+skip leaves, 12 calls across all 8 cells (§4e), of which a
   handful differ.
4. **Parse bit-identical.** `skip_txfm` is a *reconstruction* decision, not a
   parse one: `parse_decode_block` reads the var-tx tree only `if (!mbmi->skip_txfm)`
   (`decodeframe.c:1244`) but the leaf's `skip`, `use_intrabc` and DV symbols are
   read identically either way. Nothing upstream of the copy moves, which is why
   the divergence appears as pure pixels and never as a desync.

The `182` is not a DC variant on either side. It is **our** DC from an edge pair the
oracle never read (`(808 + 644 + 4) / 8`); the oracle's `202` is a bilinear copy of
a flat 202 region at DV `(col 0, row -1024)` in 1/8-pel — `av1_predict_intra_block`
is never called, so the prior reports' "which DC variant" framing is a category
error and is not revived here.

---

## 3. The copy geometry, line by line, against libaom

Traced for the seed's shape — **`BLOCK_8X4` intrabc leaf, `mi_col = 58`,
`mi_row = 40`, `ss (1,0)`**, i.e. luma x 232..239, y 160..163.

**Which libaom routine runs.** `build_inter_predictors`
(`av1/common/reconinter_template.inc:271-284`) dispatches on
`is_sub8x8_inter(xd, plane, bsize, is_intrabc_block(mi), build_for_obmc)`, and
`is_sub8x8_inter` returns **false** immediately for intrabc
(`reconinter_template.inc:85-87`). **So an INTRABC chroma block never reaches
`build_inter_predictors_sub8x8` — it goes through `build_inter_predictors_8x8_and_bigger`
(`:194-268`) even at 4x4.** That is the single most important structural fact for
this shape and it is the opposite of the intra path.

**Source origin** — libaom `reconinter_template.inc:218-223`:

```c
  const int row_start = (block_size_high[bsize] == 4) && ss_y && !build_for_obmc ? -1 : 0;
  const int col_start = (block_size_wide[bsize] == 4) && ss_x && !build_for_obmc ? -1 : 0;
  const int pre_x = (mi_x + MI_SIZE * col_start) >> ss_x;
  const int pre_y = (mi_y + MI_SIZE * row_start) >> ss_y;
```

`BLOCK_8X4`: `block_size_wide = 8 ≠ 4` ⇒ `col_start = 0`;
`block_size_high = 4` but `ss_y = 0` at 4:2:2 ⇒ `row_start = 0`.
`mi_x = 58*4 = 232`, `mi_y = 40*4 = 160` ⇒ **`pre_x = 116`, `pre_y = 160`.**

Ours, `decode.rs:25954`: `cpx = ((lmi.1 & !1) * MI) >> ss_x = (232) >> 1 = 116`,
`cpy = py >> ss_y = 160`. **Identical.**

**Per-axis DV scaling** — libaom `av1/common/reconinter.h:343-354`
(`clamp_mv_to_umv_border_sb`):

```c
  MV clamped_mv = { (int16_t)(src_mv->row * (1 << (1 - ss_y))),
                    (int16_t)(src_mv->col * (1 << (1 - ss_x))) };
```

then `decodeframe.c:624-638` (the unscaled branch, taken because intrabc uses
`cm->sf_identity`, `reconinter_template.inc:227`):

```c
    int pos_x = inter_pred_params->pix_col << SUBPEL_BITS;
    int pos_y = inter_pred_params->pix_row << SUBPEL_BITS;
    …
    pos_x += mv_q4.col;   pos_y += mv_q4.row;
```

Ours, `decode.rs:26056-26057` (and identically `:26170-26171`, `:26183-26184`):

```rust
mv_to_q4(cpx, dv.1, ss_x(fctx)),   // col axis scaled by ss_x
mv_to_q4(cpy, dv.0, ss_y(fctx)),   // row axis scaled by ss_y
```

with `mv_to_q4` (`decode.rs:37595-37598`) = `pos * 16 + mv * (1 << (1 - ss))`.
At the seed: `116*16 + 0*1 = 1856` and `160*16 + (-1024)*2 = 512`, which is
libaom's `116<<4` and `160<<4 - 2048`. **Identical, per axis, on the right axes.**

**Extent** — libaom `dec_build_inter_predictor` (`decodeframe.c:705-707`) passes
`xd->plane[plane].width/height` as `bw`/`bh`, which `set_plane_n4`
(`av1/decoder/../common/av1_common_int.h:1344-1355`) sets to
`AOMMAX((mi_size_wide[bsize] * MI_SIZE) >> ss_x, 4)` and the same for height. For
`BLOCK_8X4` at `ss(1,0)`: `(2*4)>>1 = 4` and `(1*4)>>0 = 4` ⇒ **`bw = bh = 4`.**
Ours: `predict_with_filter(…, 4, 4, …)` and a 16-sample `ub`/`vb`. **Identical.**

**Interpolation** — libaom `init_interp_filter_params` with `is_intrabc` forces
bilinear (the EIGHTTAP filter is not allowed for intrabc); ours passes
`InterpFilterKind::Bilinear`. **Identical.**

---

## 4. The four suspects, dispositioned

### (a) The DV's chroma scaling per axis — **REFUTED**

Ours already scales each axis by its own subsampling (`ss_x` on the column
component, `ss_y` on the row component), which is exactly libaom's
`src_mv->row * (1 << (1 - ss_y))` / `src_mv->col * (1 << (1 - ss_x))`. Arithmetic
shown above is identical term for term at the seed. **No change.**

### (b) The source origin's chroma rounding — **REFUTED as a defect, with a reach argument**

Our rule is `cpx = ((lmi.1 & !1) * MI) >> ss_x` — floor to an **even** mi column.
libaom's rule is `(mi_x + MI_SIZE * col_start) >> ss_x` with `col_start = -1` iff
**the luma block is 4 px wide** (`block_size_wide[bsize] == 4`), i.e. floor by
**one** mi, and only in that case. So the two agree everywhere except an **even**
mi column on a 4-px-wide luma leaf, where ours sits 2 chroma px right of libaom's.
That combination is **structurally unreachable** into this function:

* `decode_leaf_rect8` (`decode.rs:27074-27080`) sets
  `has_chroma = !vert || i == 1` at 4:2:2. The VERT split (`BLOCK_4X8`) therefore
  reads chroma on `i == 1` only, whose `lmi.1 = leaf_mi.1 + 1` is **odd** — and
  that matches libaom's own `is_chroma_reference` for `BLOCK_4X8` at `ss(1,0)`,
  which reduces to `mi_col & 1` (`av1_common_int.h:1454-1461`, with
  `!subsampling_y` true and `mi_size_wide = 1`, `mi_size_high = 2`).
* `decode_leaf_split4` (`decode.rs:26395-26404`) reads chroma at
  `(lmi.1 & 1) == 1` — **odd** — for the `BLOCK_4X4` leaf.
* The HORZ (`BLOCK_8X4`) leaves have `lmi.1 = leaf_mi.1`, and an 8x8 group's `mi_col`
  is always even, so `& !1` is a no-op and libaom's `col_start` is 0.

**Every reachable caller is either an even mi column on an 8-px-wide leaf (floor
inert) or an odd mi column on a 4-px-wide leaf (both rules floor by one mi).**
Measured: 12/12 reached calls have `leaf=(8,4)` at an **even** `mi_col` (§4e).
**No change** — and this lane does **not** re-open it.

### (c) The plane-block extent for the sub-8 leaf — **REFUTED for the reached shape; a NAMED LATENT GAP for `BLOCK_4X8`**

For `BLOCK_8X4` the extents match exactly (§3). For a **`BLOCK_4X8`** leaf at
`ss(1,0)`, libaom computes `plane.width = AOMMAX((1*4)>>1, 4) = 4` and
`plane.height = (2*4)>>0 = 8`, and `dec_build_inter_predictor` hands **that** to
`build_inter_predictors_8x8_and_bigger` — a **4x8** chroma prediction — while
`sub8_leaf_chroma422` copies an unconditional **4x4**. That is a real geometric
disagreement. Its reach on the corpus is **zero** (§4e). **I did not patch it**:
changing an unexercised arm on reasoning alone is precisely what broke
`R_odd322x240` and `V_tile2x2_odd` in the intermediate state recorded in
`lanes/av1422ctattr` §7 (4806 and 0 samples respectively). Handed over as §7 with
its reach census.

### (d) Our copy reading a neighbour unit's source — **REFUTED**

The copy reads exactly one 4x4 window at `cpx + mv*scale`, and §3 shows that window
equals libaom's `pre_x/pre_y + mv_q4` block for every reached call. A window or
offset rule that reached a neighbour unit would move whole units by multiples of
the anchor; the BEFORE census shows the wrong region is `chroma cols 116-139,
rows 160-201` — 24 of 160 columns — which is exactly the set of the 12 intrabc+skip
leaf units' footprints on `Q_odd320x242`, not a shifted-window smear. **No change.**

### (e) Reach census for the whole arm (temporary probe, removed)

`EC_422IBCPROBE=1` printing `mi`, `leaf_shape`, `cpx`, `cpy`, `dv`, `ss` at
`decode.rs`'s `SKIPPED_INTRABC_DV_COPY_HITS` arm, over all 8 cells:

| cell | hits | shapes |
|---|---|---|
| `Q_odd320x242` | 4 | all `leaf=(8,4)`, all `mi_col` even, `cpx ∈ {116,128,132}` |
| `R422_320x242` | 2 | `leaf=(8,4)`, `mi_col` even |
| `O_odd322x242` | 2 | `leaf=(8,4)`, `mi_col` even; one `dv=(-1104,-216)` (non-vertical) |
| `AB_inter_warp_odd` | 1 | `leaf=(8,4)`, `mi_col` even |
| `S_odd326x242_10b` | 2 | `leaf=(8,4)`, `mi_col` even (10-bit) |
| `R_odd322x240` | 1 | `leaf=(8,4)`, `mi_col` even, `dv=(-1096,-216)` |
| `V_tile2x2_odd` | 0 | — |
| **total** | **12** | **12/12 `leaf=(8,4)`; 12/12 even `mi_col`; 0 `BLOCK_4X8`; 0 `BLOCK_4X4`; 2 non-vertical DVs, so the column-axis scaling is exercised too** |

**The probe has been removed from the tree**; `git diff crates/ec-av1/src/decode.rs`
is empty.

---

## 5. Per-cell, per-plane, per-decode-frame measurement

`FINAL` (post-filter) decode-order dumps from both sides, wrong-SAMPLE counts
(`planecmp.py`). **BEFORE** = my worktree with the copy arm made unreachable
(`intrabc_dv.filter(|_| skip && false)`); **AFTER** = my worktree at tip.

| cell | decode frames | before Y/U/V | after total |
|---|---|---|---|
| `Q_odd320x242` | 17 | 0 / **9 603** / **14 456** | **0 (EXACT)** |
| `O_odd322x242` | 17 | 0 / **985** / **722** | **0 (EXACT)** |
| `AB_inter_warp_odd` | 43 | 0 / **3 705** / **4 366** | **0 (EXACT)** |
| `S_odd326x242_10b` | 17 | 0 / **301** / **751** | **0 (EXACT)** |
| `R422_320x242` (my fresh reproducer) | 17 | 0 / **6 337** / **8 398** | **0 (EXACT)** |
| `MYREPRO` (attribution lane's copy) | 17 | 0 / **6 337** / **8 398** | **0 (EXACT)** |
| `R_odd322x240` (control) | 17 | 0 / 0 / 0 | **0 (EXACT, held)** |
| `V_tile2x2_odd` (control) | 17 | 0 / 0 / 0 | **0 (EXACT, held)** |

Before, per decode frame 0..9, for the record:

```
R422_320x242  0/369/376  0/165/258  0/358/445  0/400/500  0/344/437  0/418/503  0/459/608  0/416/570  0/386/516  0/371/517
Q_odd320x242  0/422/379  0/313/758  0/511/560  0/522/851  0/511/636  0/619/745  0/610/992  0/538/664  0/719/1191  0/606/981
O_odd322x242  0/32/16    0/59/36    0/59/32    0/69/40    0/69/32    0/51/41    0/54/35    0/55/40    0/73/46    0/60/54
AB_inter…     0/32/32    0/61/66    0/64/78    0/112/114  0/64/64    0/62/64    0/89/85    0/116/117  0/123/121  0/111/146
S_odd…10b     0/17/32    0/20/55    0/3/25     0/27/54    0/43/68    0/41/78    0/25/43    0/5/42     0/18/39    0/5/34
```

After: **every plane of every decode frame of all eight cells is byte-identical**,
not merely low-count. **Luma is byte-exact on every frame of all four failing
cells** — the chroma-only signature, confirmed.

### Oracle-flip control, on BOTH states

One bit flipped in the oracle's own frame-0 U plane (`y=60 x=70`), same comparator:

| state | unflipped | flipped | delta | landed in |
|---|---|---|---|---|
| BEFORE (`Q_odd320x242`) | U 9 603 | U 9 604 | **+1** | U, frame 0 |
| AFTER (`Q_odd320x242`) | **0** | **1** | **+1** | U, frame 0 |

The comparator bites on the new state: exactly one flipped bit produces exactly one
extra wrong sample, in the plane the flip landed in. **The zeros in §5 are real
zeros.**

---

## 6. Regression witnesses — and why they are a control, not a coincidence

All five byte-exact against the private oracle, **on both states**:

| witness | sha256 (first 16) | geometry | BEFORE | AFTER |
|---|---|---|---|---|
| `W_intrabc` | `0aad0d6fdd236557` | 320x240, intrabc-heavy | **17/17 exact** | **17/17 exact** |
| `X_intrabc_tiled` | `e7c0c60af1a61531` | 320x240, intrabc-heavy, tiled | **17/17 exact** | **17/17 exact** |
| `Y_intrabc_10b` | `ce84d7cb4cfadf3f` | 320x240, **10-bit** | **17/17 exact** | **17/17 exact** |
| `ll422_allintra` | `d56f6b655743897a` | 320x240, 4:2:2 lossless all-intra | **1/1 exact** | **1/1 exact** |
| `ll422_noibc` | `c8559205ecf398fa` | 320x240, 4:2:2 lossless, no IBC | **16/16 exact** | **16/16 exact** |

`W_intrabc` / `X_intrabc_tiled` / `Y_intrabc_10b` are the strong control and they
hold. But **state this honestly: they are exact in the BEFORE state too, so they do
not discriminate this arm.** They prove the change does not *disturb* intrabc-heavy
material — which is what the brief asked of them — and they are not evidence that
they exercise the fixed arm. `W`/`X`/`Y` are 4:2:0/4:4:4 and never enter
`sub8_leaf_chroma422`, which is 4:2:2-only by construction.

`ll422_allintra` and `ll422_noibc` are **not committed** fixtures (the header
refusal makes 4:2:2 unreachable from a committed test, §8); I used the pinned
copies under `/home/tahinli/.cache/cells/av1422lpf/probe/`, whose hashes are listed
above so the claim is checkable.

---

## 7. Handover — the one unrepaired arm of this class, with its reach census

**`sub8_leaf_chroma422`'s copy is 4x4; libaom's chroma plane block for a
`BLOCK_4X8` leaf at `ss(1,0)` is 4x8.**

* libaom: `set_plane_n4` (`av1_common_int.h:1344-1355`) →
  `width = AOMMAX((1*4)>>1, 4) = 4`, `height = (2*4)>>0 = 8`;
  `dec_build_inter_predictor` (`decodeframe.c:705-707`) passes those as
  `bw`/`bh` into `build_inter_predictors_8x8_and_bigger`.
* ours: `decode.rs:26051-26063` and `:26165-26191` are unconditional 4x4, with
  16-sample `ub`/`vb`.
* shape is reachable in principle: `is_chroma_reference(BLOCK_4X8, ss(1,0))`
  reduces to `mi_col & 1`, and `decode_leaf_rect8`'s `has_chroma = !vert || i == 1`
  routes the odd-column `BLOCK_4X8` leaf into `sub8_leaf_chroma422`.
* **reach on the corpus: 0 of 12 calls, across all eight cells** (§4e).

It needs a **witness**, not a patch. The recipe to build one: the §1 `aomenc` R-arm
line (`--cpu-used 0 --lag-in-frames 25 --auto-alt-ref=1 --enable-global-motion=1
--cq-level=24 --kf-min-dist=0 --kf-max-dist=999999`) is what makes the partition
search land on intrabc sub-8 leaves at all (`lanes/av1422ctrigger` §2e); feed it
content whose 8x8 groups split **VERT** at an odd `mi_col` and gate on a
counter fired only when the `leaf_shape == (4, 8)` branch of this arm runs.

Also carried forward from `lanes/av1422ctattr` §10.1, which I did **not** re-open
and did **not** re-measure: `decode_leaf_split4`'s 4-leaf group tail
(`decode.rs:26732-26790`) still tests `leaf_skips[3]` **before** its `last_intrabc`
copy arm. For 4:2:2 that tail is not reached (`chroma_422` returns early at
`decode.rs:26710-26717`), so it cannot carry this defect; it remains a 4:2:0/4:4:4
question and needs its own witness.

And the correction from §0, which future lanes should not re-derive: **libaom's
`MI_SIZE` is 4** and so is ec-av1's, so `lmi.1 == mi_col` exactly. Any ladder read
on an 8-px grid is wrong by 2x and will manufacture phantom "uncovered region"
findings.

---

## 8. Gate-ability: NO. Stated plainly, not substituted.

4:2:2 is refused at the **sequence header** —
`crates/ec-av1/src/stream.rs:1803`, `if seq.subsampling_x != seq.subsampling_y`.
**No committed fixture and no committed test can reach this code.** Every
measurement in §1, §4 and §5 required patching that one guard out in a scratch
build; it is **restored** and verified restored (`stream.rs:1803` reads
`if seq.subsampling_x != seq.subsampling_y {`), and `git status --porcelain` in the
lane tree is **empty**.

The evidence offered instead is the per-plane byte-exactness census of §5 over
eight cells and 192 decode frames, plus the flip control that proves the comparator
counts, plus the mutation in §5's BEFORE column that proves the comparator and the
fix are not both vacuous. **I am not offering a source-scan substitute, and I am
not committing the bypass.**

---

## 9. Regression

`cargo test --release -p ec-av1 --lib -- 420 422 444 lossless warp intra intrabc
--skip bitrate_target_lands_within_5_percent_over_48_frames` on this lane tree at
`1686dc8a`, with the **refusal guard restored and no source change**,
`CARGO_TARGET_DIR=/home/tahinli/.cache/tgt/av1422ctintrabc`,
`EC_AV1_AOMDEC=/home/tahinli/.cache/aom-oracle/build/aomdec`, `EC_NOMEMGUARD=1`.
`/tmp` was checked first (12 G of 16 G free, `/tmp/ec-av1-*` cleared) so no gate
can fail with `Os { code: 122, kind: QuotaExceeded }`.

```
test result: ok. 181 passed; 0 failed; 2 ignored; 0 measured; 619 filtered out; finished in 253.40s
```

**`181 passed / 0 failed / 2 ignored`** — the same pass count
`lanes/av1422ctattr` §8 and `lanes/av1422ctrigger` §8 report, so nothing moved. The
filtered-out count is 619 rather than their 618 because my tip carries one further
filtered test; the pass count is identical. Green on the first attempt — no
red-then-green sequence to explain, and `/tmp` was at 12 G of 16 G free with
`/tmp/ec-av1-*` cleared, so no failure is a `QuotaExceeded` artefact.

---

## 10. Handover hygiene

* Lane tree `/home/tahinli/.cache/wt/av1422ctintrabc`, branch
  `lane-av1422ctintrabc`: **clean apart from this report.** `git status
  `--porcelain` shows only this untracked report; the reach probe, the BEFORE
  mutation and the 4:2:2 bypass are **all reverted** and `git diff
  crates/ec-av1/src/decode.rs` is empty.
* Primary checkout `/home/tahinli/Documents/Code/Rust/edith_codecs`: `git status
  --porcelain` **empty**. No relative-path leak; all edits used absolute paths
  inside the worktree.
* No push, no merge, no rustfmt. The 4:2:2 sequence-header bypass is **not**
  committed anywhere.
* The shared oracle **binary** was not rebuilt or overwritten (mtime still
  `10:36`). The shared oracle **source** WAS written to, by my mistake, at 15:14
  — full disclosure and measured blast radius in §1's *Disclosure* subsection;
  it now passes `check-aom-oracle-rungs.sh` with every assertion green and a
  generator re-run is a no-op, i.e. tree and generator agree. My private oracle
  lives at `/home/tahinli/.cache/aom-oracle-av1422ctintrabc` (outside the repo);
  its `aomdec` output is byte-identical to the shared oracle binary's on three
  cells, and it is the binary every number in this report was measured on.
* Measurement scripts, the reproducer and the dumps live outside the repo, in
  `/home/tahinli/.cache/lane-av1422ctintrabc/`.