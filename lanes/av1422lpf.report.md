# lane-av1422lpf — the `--lossless=1` 4:2:2 inter chroma panic family is SEVEN sites, and all seven are fixed

Base `aef4fa67` (main), worktree `~/.cache/wt/av1422lpf`, branch
`lane-av1422lpf`. **All seven panics are gone. The three cells still do not
decode byte-exact** — §4 says exactly where that residual lives and why it is
not this family. `crates/ec-av1/src/stream.rs` carries no committed probe
bypass; `git status` is clean and the `EC_AV1_ALLOW_422_PROBE` patch is
reverted (patch-run-restore, never committed).

## 0. Verdict

| | |
|---|---|
| sites found | **7** (the previous lane found 5; two are the intra twins of sites 1 and 5) |
| sites fixed | **7** |
| sites **not** bugs | **1** (the previous lane's "site 3" — measured, §2 site 3) |
| panics remaining on the three cells | **0** (was 3) |
| regressions on 33 other cells | **0** |
| the three cells byte-exact | **NO** — §4, with the control that names the residual's owner |

The previous lane reverted because no cell produced a right pixel. That is
still true — but the reason is now known and it is **not** this family: a
single-key-frame 4:2:2 lossless stream diverges on the **intra** path with
the *same* wrong-sample counts on this tree and on main (§4.2). Fixing seven
extent sites cannot fix that, and these seven are correct.

## 1. The repro, re-verified myself first

`main` at `aef4fa67`, all three cells, `decode_probe` with the local
`EC_AV1_ALLOW_422_PROBE` bypass:

```
W_intrabc.obu        130320 B   panicked at decode.rs:37565:24
X_intrabc_tiled.obu  131696 B   range end index 20 out of range for slice of length 16
Y_intrabc_10b.obu    202281 B   read_inter_chroma_lossless -> decode_inter_block
                               -> decode_inter_frame_tile_with_cdfs
```

The previous lane's panic text reproduced exactly. sha256 first 16 hex:

| cell | sha256 | recipe |
|---|---|---|
| `W_intrabc` | `0aad0d6fdd236557` | testsrc2 320x240 4:2:2 + `--lossless=1` |
| `X_intrabc_tiled` | `e7c0c60af1a61531` | W + `--tile-columns=1 --tile-rows=1` |
| `Y_intrabc_10b` | `ce84d7cb4cfadf3f` | W at 10-bit |

Committed as fixtures with size + `fnv1a64` pins (`0xb6d5c4653a32567a`,
`0xb6e04621d2116ef7`, `0x2ecf03435a14dbf`).

**Instrumented geometry at the failing call**, measured on this tree, not
inherited:

```
ZZLIC plane=1 ox=0 oy=0 org=(0,0) reg=(4,16) blk=(4,16) stride=4 maxidx=15 alloc=16
ZZLIC plane=1 ox=0 oy=4 org=(0,0) reg=(4,16) blk=(4,16) stride=4 maxidx=31 alloc=16
```

A **4-wide × 16-tall** chroma plane block read at `stride` 4, needing 32
elements against the square's 16.

### 1.1 The previous lane's 4:2:2-only claim is wrong

Site 1 is **not** 4:2:2-specific, and this is measurable on a stream this
decoder already decodes. A **64x128 4:2:0** block's chroma plane block is
32x64, and `stride * stride` allocates 1024 where the walk addresses 2048:

```
$ cargo run --example decode_probe ll420_a.obu     # 4:2:0, --lossless=1
panicked at crates/ec-av1/src/decode.rs:37574:24:
range end index 1028 out of range for slice of length 1024
ZZLIC ... reg=(32,64) blk=(32,64) stride=32 maxidx=1023 alloc=1024
ZZLIC ... ox=0 oy=32 ... maxidx=1123 alloc=1024      <- overran
```

This stream was never in any prior lane's corpus (the previous lane's 4:2:0
"lossless" family was encoded from a **4:2:2** y4m — `src_mandel_320x240.y4m`
carries `C422`, so those three cells were 4:2:2 all along and their "exact"
rows were measuring the wrong chroma format). I re-encoded the 4:2:0 corpus
from a real `yuv420p` source; §3 has those numbers.

## 2. The per-site table

Every site is one of: the composed grid's **allocation** (`stride * stride`),
or an **index/argument** that reads the enclosing square `chroma_side` =
`max(side >> ss_x, side >> ss_y)` where the plane block's own per-axis
`(bw >> ss_x, bh >> ss_y)` belongs. `chroma_side` equals the plane block
**exactly when `ss_x == ss_y`**, which is why every fix is inert at 4:2:0 and
4:4:4.

| site | site (function) | old | new | 4:2:0 | 4:4:4 | gate | mutation |
|---|---|---|---|---|---|---|---|
| **1** | `read_inter_chroma_lossless` composed-grid extent | `stride * stride` | `stride * blk_h` | `blk_h == stride` → identical | identical | `the_422_lossless_inter_chroma_walk_sites_stay_per_axis`, row 1 | reds: *"site 1 … is BACK"* |
| **2** | `decode_inter_block` lossless TX_4x4 unit replay | `(cr*cu+rr) * chroma_side + cc*cu` | `… * chroma_stride + …` | identical | identical | same gate, site-2 assertion | reds: *"site 2 is BACK"* |
| **3** | *(the replay's unit counts)* | `write_h >> ss_y(fctx)` | **UNCHANGED — not a bug** | — | — | asserted in the gate's geometry table | n/a |
| **4** | `decode_inter_block` strip chroma rect, VERT arm | `else { (4, 8) }` | `else if ss_y == 0 { (4, 16) } else { (4, 8) }` | `(4,8)` — identical | `(4,16)` — identical | same gate, row 3 | reds: *"site 4 … is BACK"* |
| **5** | mu-chunk lossless walk stride (single-ref + compound) | `chroma_side` | `chroma_stride` | identical | identical | same gate, row 4 (flattened form) | reds: *"site 5 … is BACK"* |
| **6** | intra-in-inter walk block shape (both call sites) | `(chroma_side, chroma_side)` | `(chroma_stride, chroma_buf_h)` | identical | identical | same gate, site-6 assertion | reds: *"site 6 is BACK"* |
| **7** | `read_intra_chroma_lossless` composed-grid extent | `stride * stride` | `stride * blk_h` | identical | identical | same gate, row 2 | reds: *"site 7 … is BACK"* |

### Site-by-site justification

**Site 1.** `u_out`/`v_out` is the block's whole chroma **plane block** read at
`stride`, and libaom's plane block is per-axis. The walk addresses row
`org_y + reg_h - 1` at column `org_x + reg_w`; every one of the nine call
sites passes a region that is a sub-rect of `blk` with `org` inside `blk`, so
the requirement is exactly `stride * blk_h` (the last row's end
`(blk_h-1)*stride + blk_w` fits because `blk_w <= stride` at all nine). Where
the plane block is square — 4:2:0 and 4:4:4 on a square block — `blk_h ==
stride` and this reduces to the old expression **verbatim**. Measured panic
removed: `range end index 20 … length 16` gone on all three cells, **and**
`range end index 1028 … length 1024` gone on the 4:2:0 stream.

**Site 2.** The composed grid this replays is the plane block at
`chroma_stride` — the same stride site 1 sized it at and the same stride
`read_inter_chroma_lossless` wrote it at. `chroma_side` at 4:2:2 is the LUMA
side, twice a strip block's plane-block width. Measured: `range end index 68
out of range for slice of length 64` on `W_intrabc` frame 1.

**Site 3 is NOT a bug — the previous lane's site 3 was a misreading.** It
proposed changing the unit counts

```rust
let (rows, cols) = ((write_h >> ss_y(fctx)).div_ceil(cu), (write_w >> ss_x(fctx)).div_ceil(cu));
```

These are *already* per-axis: `>> ss_y` and `>> ss_x` are the plane block's own
axes, so `rows` counts chroma rows at 4:2:2 exactly as at 4:2:0. Measured on
the failing cell: `write=(8,16) ss=(1,0)` gives `rows=4, cols=1` against a
`(4,16)` plane block — correct. Applying the previous lane's
`(chroma_buf_h, chroma_stride)` rewrite would have been a no-op at best.
**Not changed.** The gate asserts the per-axis geometry table instead.

**Site 4.** A `PARTITION_VERT_4` strip is 4x16 luma and pairs on **columns**
while `ss_x == 1` (`is_chroma_reference` on `mi_col` parity,
`av1_common_int.h:1454`), so the pair is 8x16 luma. 4:2:2 subsamples X only,
which leaves the pair's plane block at `8>>1 x 16>>0` = **4x16**, twice as
tall as 4:2:0's 4x8. Measured: a 4x16 vert strip got `chroma_stride = 4`,
`chroma_buf_h = 8`, a 32-sample grid, and the replay's last unit row read
index 64 of it. The HORZ arm needs **no** third shape at 4:2:2 and gets none:
a HORZ_4 strip pairs on rows only while `ss_y == 1`, and 4:2:2 has `ss_y == 0`,
so each 16x4 strip is its own chroma reference with its own `(8>>1, 4>>0)`
= (8,4) plane block — the number the existing arm already returned. The gate
asserts both of those geometries, so a future "simplification" that folds
the VERT arm back cannot pass silently.

**Site 5.** The stride the walk indexes `su`/`sv` at is the **prediction
buffer's own** `chroma_stride` — the number `su`/`sv` were built at and the
number the composed grid is sized and written at (site 1). Measured on
`X_intrabc_tiled` frame 1: `cpx=64 cpy=0 reg=(32,64) blk=(64,128) ss=(1,0)`,
the walk addressed `oy*128 + ox` on a 64-wide 8192-sample buffer and the last
unit row ran off the end (`offset 8192 .. 8580 of 8192`).

**Site 6.** The intra-in-inter twin of site 5. `palette_uv_bufs` is the decoded
colour-index map at `(write_w >> ss_x, write_h >> ss_y)` — per-axis already —
and the lossless walk windows it at this `stride`. Measured on
`Y_intrabc_10b.obu`: `buf_len=128 stride=16 org=(0,8)` put the third unit row
at index 180 (`palette_window`, `decode.rs:2946`).

**Site 7.** `read_intra_chroma_lossless` carries site 1's defect verbatim,
including its own `(blk_w, blk_h)` parameter, so the intra twin gets
`stride * blk_h` too. Measured: `range end index 36 out of range for slice of
length 32`.

## 3. End state, per cell, with controls

Comparator: the crate's own `count_rawvideo_diffs` logic (via a throwaway
sweep harness modelled on it — oracle `aomdec --rawvideo` bytes read
directly, per-frame plane attribution from each decoded frame's own plane
lengths). A pristine-main binary was built in a separate worktree
(`~/.cache/wt/av1422base`, same harness, same probe bypass) so every number
below is a **diff against main**, not a claim.

### 3.1 The three panic cells

| cell | main `aef4fa67` | this lane |
|---|---|---|
| `W_intrabc` | **PANIC** `decode.rs:37565` | Golomb-tail refusal at frame 2 |
| `X_intrabc_tiled` | **PANIC** `decode.rs:37565` | Golomb-tail refusal at frame 2 |
| `Y_intrabc_10b` | **PANIC** `decode.rs:37565` | decodes 16/16, diverges Y 2 069 389 / U 1 031 181 / V 1 084 387 |

All three panics are gone. None is byte-exact. §4 says why.

### 3.2 Zero regressions — the full 33-cell diff

Every cell re-run on both trees; `diff` of the two tables is **empty** except
the three cells above.

| family | cells | main | this lane |
|---|---|---|---|
| 4:2:2 committed pins | `422_allskip_2f`, `422_intrabc_sb128_strip`, `…_notxsearch`, `422_residual_compound_warp_16f`, `…_nolr_16f`, `422_sb128_3f` | 6/6 EXACT | **6/6 EXACT** |
| 4:2:2 lossy corpus | `AA_inter_compound`, `AD_inter_nogm`, `A`,`B`,`C`,`D`,`E`,`F`,`H`,`I`,`J`,`K`,`L`,`R`,`T`,`U`,`V` | 17/17 EXACT | **17/17 EXACT** |
| 4:2:2 pre-existing divergences | `AB_inter_warp_odd`, `O_odd322x242`, `Q_odd320x242`, `S_odd326x242_10b` | DIVERGES | **DIVERGES, identical counts** |
| 4:2:0 lossless (re-encoded from a real `yuv420p` source) | `ll420_a/b/c/d` | 4/4 EXACT | **4/4 EXACT** |
| 4:4:4 lossless | `ll444_a`, `ll444_b` | 2/2 EXACT | **2/2 EXACT** |
| 4:4:4 lossless + altref | `ll444_c` | DIVERGES 7/16 | **DIVERGES, identical counts** |
| 4:2:0/4:4:4 all-intra lossless controls | `ll420_allintra`, `ll444_allintra` | 2/2 EXACT | **2/2 EXACT** |

### 3.3 The site-4 lossy re-quote (the regression surface)

Site 4 is the one change with a real regression surface: it alters
`chroma_stride`/`chroma_buf_h` for the **whole** of `decode_inter_block` at
4:2:2, including the LOSSY arms. Re-quoted individually, not as a set:

| cell | shape exercising site 4 | main | this lane | bytes |
|---|---|---|---|---|
| `422_residual_compound_warp_16f` | vertical-midpoint compound + warp | EXACT 16/16 | EXACT 16/16 | 0 wrong |
| `422_residual_compound_warp_nolr_16f` | same, LR off | EXACT 16/16 | EXACT 16/16 | 0 wrong |
| `422_sb128_3f` | sb128 skip-heavy inter | EXACT 3/3 | EXACT 3/3 | 0 wrong |
| `AA_inter_compound` | compound inter, 40 frames | EXACT 40/40 | EXACT 40/40 | 0 wrong |
| `AD_inter_nogm` | inter, no global motion, 40 frames | EXACT 40/40 | EXACT 40/40 | 0 wrong |
| `L_tiled` / `T_tilecols2` / `U_tilerows1` / `V_tile2x2_odd` | tiled inter | EXACT 16/16 each | EXACT 16/16 each | 0 wrong |
| `AB_inter_warp_odd` | warp on odd dims | DIVERGES U 3 642 / V 10 186 | **identical** | unchanged |

**Site 4 is not a regression.** Every lossy 4:2:2 arm that measured
byte-exact on main still measures byte-exact, to the sample, and the one
pre-existing divergence is unchanged in magnitude.

### 3.4 Gates, with mutation proof

`the_422_lossless_inter_chroma_walk_sites_stay_per_axis` — one assertion per
site. Every scan assertion has a **forbidden twin** (the square-cut spelling
is absent) *and* a **required twin** (the per-axis spelling is present), so
neither an unreverted site nor a matcher that stopped matching can pass.

| mutation | result |
|---|---|
| site 1 → `stride * stride` | **RED** *"site 1 … is BACK"* |
| site 7 → `stride * stride` | **RED** *"site 7 … is BACK"* |
| site 2 → `chroma_side` | **RED** *"site 2 is BACK"* |
| site 4 → drop the `ss_y == 0` arm | **RED** *"site 4 … is BACK"* |
| site 5 → `chroma_side` at both mu-chunk walks | **RED** *"site 5 … is BACK"* |
| site 6 → `(chroma_side, chroma_side)` | **RED** *"site 6 is BACK"* |

**Two matchers were dead on first write and are named as such.** Sites 5 and
6 initially passed their own revert mutation: the literals being searched for
never matched even *with* the fix in place, because a line-anchored matcher
cannot span the comment the fix itself writes into its argument list. That is
the `dead-matcher-positive-control` class, caught by running the mutation
rather than trusting the green. The gate now builds a **whitespace-flattened**
second form of each body for the matchers that span an argument list, and both
re-run mutations now red. A gate I did not mutate would have shipped two holes
in it.

### 3.5 Gates run, scoped

Clean tree at `69a3a4df`:

- `the_pinned_422_lossless_inter_chroma_panics_refuse_by_name` — pass
- `the_422_lossless_inter_chroma_walk_sites_stay_per_axis` — pass
- `the_pinned_422_intrabc_sb128_strip_witnesses_refuse_by_name` — pass
- `the_pinned_422_bigblock_witnesses_are_present_and_refuse_by_name` — pass
- `the_pinned_422_lr_off_witness_is_present_and_refuses_by_name` — pass
- `the_pinned_422_residual_compound_warp_witness_is_present_and_refuses_by_name` — pass
- `a_lossless_libaom_inter_frame_decodes_sample_exact` — pass
- 4:4:4 family (`444`, `lossless`, `ll444` filters) — **62 passed, 0 failed**
- 4:2:0 family (`pixel_exact_420`, `420` filters) — **6 passed, 0 failed**
- oracle binary `~/.cache/aom-oracle/build/aomdec` (instrumented, rung 12)

## 4. What is NOT byte-exact, and the control that names its owner

The charter asked for the three cells byte-exact. **They are not.** The
residual is real, and the measurement below locates it **outside** this family.

### 4.1 Two of them refuse; one decodes and diverges

| cell | symptom | located at |
|---|---|---|
| `W_intrabc` | `a Golomb tail longer than this decoder reads` | mi (16,52) |
| `X_intrabc_tiled` | same | mi (8,64) |
| `Y_intrabc_10b` | decodes all 16 frames, diverges from frame 0 byte 16 (luma, got 40 want 68) | frame 0, first luma sample |

A Golomb-tail refusal is always the **symptom** of an earlier desync (the code
says so at `decode.rs:7965`), never a reader gap.

### 4.2 The control that decides it

A **single-key-frame**, `--lossless=1` 4:2:2 stream with **no inter content
at all** — nothing in this family is reachable — diverges identically on this
tree and on main:

| cell | main | this lane |
|---|---|---|
| `ll422_allintra` (1 key frame, all-intra, 4:2:2 lossless) | DIVERGES **Y 75 210 / U 38 234 / V 38 207** | **Y 75 210 / U 38 234 / V 38 207** — identical |
| `ll420_allintra` (same recipe, 4:2:0) | EXACT | EXACT |
| `ll444_allintra` (same recipe, 4:4:4) | EXACT | EXACT |

**Byte-identical wrong-sample counts on the same recipe at two chroma
formats and clean at the other two.** The 4:2:2 lossless **intra** path
diverges from `aomdec` on main, before any inter chroma walk runs, and my
seven extents neither caused nor changed one sample of it. That is a
pre-existing defect with its own owner; chasing it here would be the
"substitute an easier problem" failure, and closing it is not what makes these
seven extents correct.

`Y_intrabc_10b` diverges from **frame 0 byte 16** — its first key frame, i.e.
the same intra path. `W`/`X` reach frame 2 before their entropy desync, which is
where a *later* inter defect can still be hiding; I have not excluded that a
second inter-side defect exists behind the intra one. That is named in
`not_done`, not claimed clean.

## 5. `not_done`

- **The three cells do not decode byte-exact.** Two refuse with a Golomb-tail
  desync at frame 2, one diverges from frame 0. Exact reason and control: §4.
- **The 4:2:2 lossless INTRA path diverges from `aomdec`** (byte-identical on
  main and here, §4.2) — pre-existing, not in this family, **not fixed here**.
  It must be fixed before any of the three cells can reach byte-exact, and it
  is a separate lane.
- **A possible second inter-side defect behind the intra one is NOT excluded.**
  `W`/`X` desync at frame 2, and an intra-only stream already diverges, so the
  frame-2 desync cannot yet be attributed. Only "no panic" is claimed.
- **Site 4's HORZ arm was measured, not exercised by a witness.** The gate
  derives its (8,4) geometry; no pinned stream is cited as hitting it.
- **Decode-order (rung 12) comparison was not run on the three cells** — they
  do not decode to completion, so there is nothing to zip against the oracle's
  per-frame dumps. Presented-order only, and only for the cells that complete.
- **Full suite not run** (lane rules: scoped only; project-wide validation is
  Main's).

## 6. Instrumentation disclosure

Five temporary `eprintln` probes (`ZZLIC`, `ZZLIC2`, `ZZLIC3`, `ZZLIC4`,
`ZZLIC5`, `ZZGOL`) and one throwaway `examples/zz_sweep.rs`, **all removed**
before the commit — `crates/ec-av1/src/decode.rs` at `6d816368` contains only
the seven fixes and their comments. The `EC_AV1_ALLOW_422_PROBE` bypass at
`stream.rs` is patch-run-restore and is in **no** commit; `git status` is
clean. Nothing was added to the decoder or the oracle beyond the seven
expression changes.
