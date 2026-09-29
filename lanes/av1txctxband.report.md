# lane-av1txctxband — `get_tx_size_context` must read the block covering the ABUTTING CELL, not the last block written in the column

Base / branch: `a1f818a9` (main, "Merge lane-av1sb128fork: the sb128 inter fork
is a CDF-ROW error, not var-tx enumeration") → `lane-av1txctxband`, worktree
`~/.cache/wt/av1sb128fork`. The fix lane for
`lanes/av1sb128fork.report.md`'s narrowed statement.

Build: `CARGO_TARGET_DIR=/home/tahinli/.cache/tgt-av1txctx` (private).
Oracle: `~/.cache/aom-oracle/build/{aomenc,aomdec}` (instrumented), read-only,
`--threads=1` (rung order identical with and without).

---

## 0. Verdict

**The band was wrong and the fix is in.** `get_tx_size_context` read a
per-COLUMN `above_inter`/`left_inter` band (and the matching per-column
`above_side_mi`/`left_side_mi`) where libaom reads the block **covering the
abutting mi cell**. On the witness the 8x4 intra leaf at mi(57,52) took the
"an inter neighbour contributes its BLOCK size" override with a **stale side of
4** where the covering block's own width is **8**, so `above` resolved 0 instead
of 1, `ctx` came out 1 instead of 2, and `tx_size_cat0[1]` was read where
libaom reads row 2.

| | red-before | green |
|---|---|---|
| witness per-plane wrong (frame 0) | Y 841 / U 1295 / V 1372 of 65536 | **0 / 0 / 0** |
| witness per-plane wrong (frame 1) | Y 56986 / U 55974 / V 56442 | **0 / 0 / 0** |
| `EC_SYMR` ladder | 41245 ours vs 37229 oracle, fork at read **32813** | **37229 / 37229, zero divergence over the whole stream** |
| the fork read's `cdf0` | ours 6424 (`32768-6424 = 26344`) vs oracle 19938 | reconciled — the read is no longer divergent |
| `EC_TXCTX mi=57,52` | `above=false left=true` (ctx 1) | `above=true left=true` (ctx 2) = the oracle's row |
| 99-cell class sweep (§4) | 12 forked, 4 of those also `OURS-EXIT1` | **99/99 exact-entropy, no exits** |

---

## 1. Red-before, measured on the unpatched tree

Branch `lane-av1txctxband` at `a1f818a9`, no source change. The witness
(`t256_n2_cq62.obu`, 6186 B, sha256 `14acb74e825a2bb3ef195a52cc2abc67794d9c4d92080b66c739d318177fc7c6`),
compared per plane against `aomdec --threads=1` with
`EC_AV1_FINAL_DUMP` (`~/.cache/sb128fork/cmp444.py`, 30 lines, outside the repo):

```
frame 0: Y 841/65536 | first Y(216,224) ours 171 oracle 170
       | U 1295/65536 | first U(216,224) ours 139 oracle 159
       | V 1372/65536 | first V(216,224) ours 17 oracle 9
frame 1: Y 56986/65536 | U 55974/65536 | V 56442/65536
VERDICT: DIVERGENT   (exit 1)
```

The first wrong sample is `Y(216,224)` = mi(54,56) — inside SB row 1, col 1, the
superblock the forked leaf sits in.

The entropy fork, same tree, `EC_SYMR` both sides, CDF-row convention reconciled
(`a == b or 32768 - a == b`):

```
*** FORK at read index 32813 ***
ours  pre=(7550,45492,45418) cdf0=6424  n=2 s=1 post_rng=36377
aom   pre=(7550,45492,45403) cdf0=19938 n=2 s=1 post_rng=55054  site=decodeframe.c:1106
```

Identical pre-state, identical width, identical symbol, only the row — and
`32768-6424 = 26344 != 19938`, so the ICDF mirror convention does not explain
it either.

---

## 2. The rule, with the libaom lines I followed

`get_tx_size_context` — `av1/common/pred_common.h:348-380`:

```c
const MB_MODE_INFO *const above_mbmi = xd->above_mbmi;
const MB_MODE_INFO *const left_mbmi  = xd->left_mbmi;
const TX_SIZE max_tx_size = max_txsize_rect_lookup[mbmi->bsize];
const int max_tx_wide = tx_size_wide[max_tx_size];
const int max_tx_high = tx_size_high[max_tx_size];
const int has_above = xd->up_available;
const int has_left  = xd->left_available;
int above = xd->above_txfm_context[0] >= max_tx_wide;
int left  = xd->left_txfm_context[0]  >= max_tx_high;
if (has_above) if (is_inter_block(above_mbmi)) above = block_size_wide[above_mbmi->bsize] >= max_tx_wide;
if (has_left)  if (is_inter_block(left_mbmi))  left  = block_size_high[left_mbmi->bsize]  >= max_tx_high;
```

The two neighbours, and why they are PER CELL:

- `av1/common/av1_common_int.h:1380-1389` (`set_mi_offsets`):
  `xd->above_mbmi = xd->mi[-xd->mi_stride];` and `xd->left_mbmi = xd->mi[-1];`
- `av1/decoder/decodeframe.c:355-361` (`setup_mbs`):
  ```c
  for (int x = 1; x < x_mis; ++x) xd->mi[x] = xd->mi[0];
  int idx = mi_params->mi_stride;
  for (int y = 1; y < y_mis; ++y) {
      memcpy(&xd->mi[idx], &xd->mi[0], x_mis * sizeof(xd->mi[0]));
      idx += mi_params->mi_stride;
  }
  ```
  `MB_MODE_INFO::bmi` is a POINTER, so this writes the covering block's own
  pointer into **every** mi cell the block covers. `xd->mi[(r)*stride + c]` is
  therefore "the block covering (r, c)", and `xd->mi[-stride]` / `xd->mi[-1]`
  are the blocks whose bottom/left edge abuts this block's top/right edge **at
  this cell**.
- `av1/common/blockd.h:372-374`: `is_inter_block(mbmi) = is_intrabc_block(mbmi)
  || mbmi->ref_frame[0] > INTRA_FRAME` — an INTRABC block counts.

**What the old code was reading.** `tx_size_context_txfm_rect` and
`tx_size_context_txfm` (both in `crates/ec-av1/src/decode.rs`) resolved the
override as:

```rust
if has_above && n.above_inter[mi_c] { above = usize::from(n.above_side_mi[mi_c]) >= own_w; }
if has_left  && n.left_inter[mi_r]  { left  = usize::from(n.left_side_mi[mi_r])  >= own_h; }
```

`above_inter`, `left_inter`, `above_side_mi` and `left_side_mi` are all indexed
by a BARE mi column/row (`mi_c` / `mi_r`), i.e. they are **per-column bands
holding the LAST block written in that column**. They are the covering block
only for the block immediately below/right of the last writer. On the witness
at mi(57,52) the band said `above_inter=true, above_side=4` while the covering
block of cell (56,52) is 8 px wide, so the override fired with 4 and resolved
`above = (4 >= 8) = 0`.

**Measured, from the `EC_TXCTX` rung extended with the per-cell terms (the rung
now prints both models side by side so a future divergence names which one
disagreed):**

```
before: EC_TXCTX mi=57,52 own=8x4 ... above_inter=true left_inter=false \
                 above_side=4 left_side=16 above=false left=true
after:  EC_TXCTX mi=57,52 own=8x4 ... above_inter=true left_inter=false \
                 above_side=4 left_side=16 above=true  left=true \
                 | cell_inter ab=true lb=false ab_side=8 lb_side=16
oracle: EC_TXCTXB mi=57,52 bsize=2 maxw=8 maxh=4 hasup=1 hasleft=1 \
                 abv=8 lft=8 above=1 left=1 ctx=2
```

The boolean agreed all along; the SIZE term was the stale one.

## 3. The change

Four hunks, all in `crates/ec-av1/src/decode.rs`, plus one fixture and one gate.

1. **`Neighbours::inter_grid: Vec<bool>`** (new field, per 4x4 mi cell, same
   superblock-padded shape as `blk_grid`). libaom's `is_inter_block` of the
   block covering each cell.
2. **Three writers, and only the three that already wrote the bands**, so the
   per-cell grid inherits the bands' exact coverage and call-site ordering:
   - `record_inter_rect_mi` → `mark_inter_grid_rect(at_mi, w_mi, h_mi, is_inter)`
     (every block on an inter frame, with its own flag)
   - `record_intrabc_mi_rect` → `.. = true` when `dv.is_some()` (an INTRABC block
     counts as inter)
   - `fill_lf_grid_rect`'s intra-only clear → `.. = false` (every block on a key
     frame)
   It is a **separate** writer from `fill_skip_grid_rect` on purpose: the two
   publishers run in BOTH orders at different call sites (e.g.
   `record_inter_rect_mi` at `decode.rs:44824` runs BEFORE
   `fill_skip_grid_rect` at `:44844`), and they write disjoint fields, so the
   order cannot matter.
3. **The covering block's own size** comes from `blk_grid`'s existing per-mi
   `MI` — libaom's `block_size_wide[above_mbmi->bsize]` /
   `block_size_high[left_mbmi->bsize]`. No new size array: `dim` was already
   there, per mi, published by every coded block.
4. **Both readers** now gate and size the override from
   `inter_at(mi_r - 1, mi_c)` / `inter_at(mi_r, mi_c - 1)` and
   `blk_side_at(...)` instead of the four bands. `above_txfm` / `left_txfm` (the
   real `TXFM_CONTEXT`) and the `has_above` / `has_left` tile-border guards are
   **unchanged** — only the inter-neighbour override's two inputs move.

Plus `crates/ec-av1/fixtures/444_lossy_sb128_txctx_witness.obu` (6186 B,
sha256 `14acb74e…`) and the gate
`a_444_lossy_sb128_txctx_reads_the_covering_block_per_cell`.

**Why `--sb-size=128` and cq >= 54 are load-bearing for the witness** (stated on
the gate so nobody re-encodes it wrong): 256x256 is the smallest geometry that
gives `--sb-size=128` a SECOND superblock row and column, which is where a
column band and a per-cell cover stop coinciding — at the default 64 the columns
are 4 mi wide, so the two models agree for every 4-column run. The cell is
entropy-exact at cq 20..52 and forks from cq 54 up on 256x256.

## 4. Proof

**The witness, per plane** (`cmp444.py`, the same script as §1):

```
frame 0: Y 0/65536 | U 0/65536 | V 0/65536
frame 1: Y 0/65536 | U 0/65536 | V 0/65536
VERDICT: EXACT
```

**The whole entropy ladder, not just the fork read:**

```
ours 37229 reads | oracle 37229 reads
no divergence within the shorter stream
```

**The class sweep** — 10 sources x 11 cq levels = **99 cells** of 4:4:4 lossy
`--sb-size=128` (256x256 2/3/4 frames, 320x240, 256x384, 192x256, 256x192,
384x384, mandelbrot, testsrc; cq 30..62), aligned on
`(value, range, n, s, post_rng)` with the CDF-row convention reconciled
(`hunt5.py`): **99/99 `EXACT-ENTROPY`**. Before the fix the same 99 cells gave
**12 forks** (§ `lanes/av1sb128fork.report.md` §2) and **4 `OURS-EXIT1`** — the
refusals at `s256_3_cq56`, `s256_3_cq63`, `sz_320x240_cq54`,
`sz_256x384_cq54` were this same desync reaching a named refusal, and all four
now decode entropy-exact. I did not diagnose which refusal each was; the claim
is only that they no longer fire.

**The gate's non-vacuity, by mutation.** Reverting the RECT reader's four lines
to the band form and re-running the gate:

```
panicked at crates/ec-av1/src/stream.rs:8359:
  no `get_tx_size_context` read on this stream had the per-CELL cover and the
  per-COLUMN band disagree ...
test result: FAILED. 0 passed; 1 failed
```

So the counter arm is not green on the old code. Green run: **1** diverging
read — the single one at mi(57,52), named in the gate's own message. The pixel
arm is independently red on the same mutation, by the §1 measurement.

**The counter counts RESOLVED terms, not booleans** — my first version compared
`inter_at(cell) != above_inter[mi_c]` and read **0** on the witness, because on
this stream the two booleans AGREE and it is the SIZE term that is stale. The
gate's red-then-green on that first version is what caught it; the counter now
compares the resolved `above`/`left` under each model, which is the term that
feeds `ctx`.

**Neighbouring families, re-quoted on the fixed tree**
(`cargo test -p ec-av1 --lib -- <filter> --test-threads=1`, scoped):

| filter | result |
|---|---|
| `444_lossy 444_rect 444_intrabc_rect4 ll444 lossless intrabc_rect` | **44 passed, 0 failed** |
| `420 sb128 tile_rows tilerows intrabc av1real_aomenc` | **43 passed, 0 failed, 2 ignored** — the 2 ignored are `sb128_r2_control_sb64` and one sibling, both pre-existing `#[ignore]`s |

## 5. Class sweep: the other `get_tx_size_context` consumers

`grep` for every read of the four bands inside a tx-size context, and for every
other consumer of `above_inter` / `left_inter` / `above_side_mi` /
`left_side_mi`:

| site | reads | verdict |
|---|---|---|
| `tx_size_context_txfm_rect` | the override | **FIXED** — per-cell cover |
| `tx_size_context_txfm` (square) | the override | **FIXED** — per-cell cover |
| `av1_get_intra_inter_context` (`is_inter` context, spec 8.4.2.1) | `above_inter[mi_c]` / `left_inter[mi_r]` | **NOT this defect.** libaom's `av1_get_intra_inter_context` (`pred_common.c:124-146`) also reads `above_mbmi`/`left_mbmi`, so it has the same granularity hazard — but its own gate (`lane-t900`'s, `INTRA_IN_INTER_*`) and the 99-cell sweep give no evidence either way, and I did not change it. **Named, not swept.** |
| `modes_above_left` / `record_mode_mi` consumers | `above_mode`/`left_mode` (mode bands) | Unrelated field; not touched. |
| `ref_ctx` family (`above_ref`, `above_ref1`) | `ref_frame` bands | **Same hazard, same granularity question.** `is_inter_block` and `av1_get_pred_context_single_ref_*` both consult the neighbour's reference. Not changed; the 99-cell sweep is exact, so no committed fixture observes it. **Named.** |
| `txfm_partition_ctx_rect` (the inter var-tx split) | `above_txfm` / `left_txfm` only | **Correct and untouched.** libaom's `txfm_partition_context` (`av1_common_int.h:1539-1552`) reads `above_txfm_context + mi_col` / `left_txfm_context + mi_row` — the TXFM bands at mi granularity, no block-size override at all. This is the "other `get_tx_size_context` consumer" the sweep asked about, and it is right as it stands. |

So: **two** consumers of the inter-neighbour override, both fixed; **two** other
consumers of the same per-column `is_inter` bands (`is_inter` context, `ref`
context) that share the granularity hazard and are **named, not swept**; **one**
(the inter var-tx `txfm_partition_context`) that does not use them and is
correct.

## 6. Follow-up that belongs to the merge, not to this lane

`scripts/fixture-library.tsv` is GENERATED output
(`scripts/gen-fixture-library.sh`, "do not hand-edit") and it records a
`file:line` for **every** fixture reference in the crate. Regenerating it here
produced a **501-line** diff (205 insertions, 296 deletions) that has nothing to
do with this fix and would collide with every sibling lane that also touched
`decode.rs` / `stream.rs`. I reverted it and left the manifest alone: the new
pin is committed, reachable, and sha256-asserted by the gate, and the manifest
must be regenerated **once, after the wave lands**
(class `generated-manifest-regenerate-on-merge`). For the record, the row the
regeneration produces for the new pin is:

```
crates/ec-av1/fixtures/444_lossy_sb128_txctx_witness.obu	crates/ec-av1/src/stream.rs:8339	captured	-	ok	no	14acb74e825a2bb3ef195a52cc2abc67794d9c4d92080b66c739d318177fc7c6
```

## 7. What I did NOT do

- **No sweep of the `is_inter` / `ref` context consumers** (§5). They share the
  granularity hazard; nothing in the 99-cell sweep or the re-quoted families
  observes them, so changing them would be a blind refactor.
- **No diagnosis of the four `OURS-EXIT1` cells** the fix also cured. The claim
  is that they no longer exit, not why they did.
- **No change to the `above_inter` / `left_inter` / `above_side_mi` /
  `left_side_mi` bands themselves.** They still have their other readers (§5);
  deleting them is a separate cleanup with its own re-quote.
- **The `EC_PART` rung's `mi` misprint** found in the previous lane (one of its
  four arms prints the 16-px-unit index) is still there. Diagnostic-rung defect,
  not a decode defect, not this lane's fix.

## 8. Files

- `crates/ec-av1/src/decode.rs` — `inter_grid` field + constructor entry,
  `mark_inter_grid_rect` / `inter_at` / `blk_side_at`, the three writer calls,
  both readers, `TXCTX_INTER_CELL_BAND_DIVERGENCE_HITS` + accessors, and the
  `EC_TXCTX` rung extended with the per-cell terms.
- `crates/ec-av1/src/stream.rs` — the gate
  `a_444_lossy_sb128_txctx_reads_the_covering_block_per_cell` (+94 lines).
- `crates/ec-av1/fixtures/444_lossy_sb128_txctx_witness.obu` — new pin.
- Throwaway, outside the repo: `~/.cache/sb128fork/` (`cmp444.py`, `align.py`,
  `hunt5.py`, the `rb/` red-before and `gr2/` green dumps, and the 99 encodes).
