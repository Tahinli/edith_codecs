# lane-av1txctxband2 — the missing `TXFM_CONTEXT` PUBLISH: `read_block_tx_size`'s lossless early return skipped libaom's `set_txfm_ctxs`, so a mixed-lossless 640x480 frame's abutting 8x8 intra block left a stale band and the cell desynced

Base `main` = `0bfee517`. Worktree `/home/tahinli/.cache/wt/av1txctxband2`,
target dir `$HOME/.cache/cargo-target-av1txctxband2`, `EC_NOMEMGUARD=1`. Oracle
`~/.cache/aom-oracle/build/{aomdec,aomenc}` (shared, untouched; not rebuilt, not
re-instrumented). ffmpeg 8.1.3.

**Handover taken:** `lanes/av1mixllconj.report.md` §4.3 and §11
`deferred(unblock: identify the TXFM_CONTEXT band write libaom makes above
mi=(80,110) …)`, plus the same claim independently re-measured by
`lane-av1offtile` §6b.

## 0. Verdict

| | |
|---|---|
| The cause | **`read_block_tx_size`'s lossless early return returned without publishing anything.** libaom's `parse_decode_block` is an EITHER/OR: the var-tx branch's condition carries `!xd->lossless[segment_id]` as a conjunct, and the `else` branch runs `read_tx_size` **and then `set_txfm_ctxs`**. `read_tx_size` returns `TX_4X4` on its FIRST line — the early return skips the SYMBOL READ, not the PUBLISH. |
| The offending block | the **8x8 INTRA lossless block at mi(78,110)**, abutting the reader. libaom stamps `tx_size_wide[TX_4X4] == 4` into `above_txfm_context[110..111]`; we wrote nothing, so the band still held the **16** a skipped 16-px inter block at mi(36,108) had written 43 mi rows earlier. |
| The reader that broke | the 8x4 intra leaf with `use_filter_intra` at **mi=(80,110)**, `tx_size_cat0[2]` where libaom reads row 1. |
| `420_mixll_altref_640x480_5f.obu` (87537 B) | **REFUSED** at base → **6/6 decode-order frames byte-exact**; whole-stream paired `EC_SYMR` ladder **457 907 / 457 907 bit-identical, zero divergence** |
| `420_mixll_altref_640x480_6f.obu` (115615 B) | **REFUSED** at base → **7/7 decode-order frames byte-exact** |
| Corpus sweep, 110 committed fixtures | **EXACT 100 → 100, REFUSED 10 → 10, RED 0 → 0, FRAMECOUNT 0 → 0** — all ten refusals are the pre-existing by-name 4:2:2 chroma refusal. **Zero rows change**: the fix is a strict no-op on everything already correct, and it is the two NEW 640x480 fixtures that go REFUSED → 6/6 and 7/7 EXACT. |
| Recipe's closed cells | re-measured with the fix: 256x128 pin **6/6 EXACT**, 176x144 **6/6 EXACT**, 352x288 **6/6 EXACT**, the 4:4:4 arm green in the corpus (§6.1) |
| Class sweep | 5 sites of `context-band-not-published`; **1 was this defect (fixed), 1 is a real libaom-fidelity gap that IS reached but has NO observable effect on any fixture measured (`read_block_tx_size_rect`'s lossless arm, missing the `skip && is_inter` term — 29/1/9/6/16 measured hits), 2 are unobservable by construction, 1 is a deliberate gate that measurement clears** (§7) |
| Fix / fixture / counter / gate | all four landed, with a mutation proof (§5) |

## 1. Reproduction

```text
ffmpeg -f lavfi -i testsrc2=size=640x480:rate=25 -frames:v 5 -pix_fmt yuv420p src640.y4m
aomenc --codec=av1 --obu -o 420_mixll_altref_640x480_5f.obu --passes=1 --cpu-used=4 \
       --limit=5 --end-usage=q --cq-level=0 --aq-mode=1 src640.y4m
```

| cell | bytes | sha256 |
|---|---:|---|
| `420_mixll_altref_640x480_5f.obu` | 87 537 | `3d505fd2e0bbb7ca3680a8e338d712a81defc947b955a4b7b82de6718301e73c` |
| `420_mixll_altref_640x480_6f.obu` (`--limit=6`) | 115 615 | `c82ba4d793d378cc7e4a234329be87c5c03a283c8b6a202acf4d0704ea66ff7f` |

Both byte counts and both shas reproduce the inherited reports' on the first
try. At `0bfee517` the 5-frame cell decodes to

```text
TILING: 7 frame headers parsed
REFUSED: unsupported: AV1 tile (a Golomb tail longer than this decoder reads)
frames_dispatched: 0
```

— reproduced verbatim, on my own worktree and target dir.

## 2. The write trace for mi column 110

`EC_TXUPD` (the existing `set_txfm_ctxs` / `txfm_partition_update_rect` rung)
restricted to writes whose column span covers mi column 110 with `mi_r < 80`
shows only four writes, the last a skipped 16-px inter block — the report's
finding, and it is **true but incomplete**: the per-leaf publishes inside
`decode_leaf_rect8` and `decode_intra_rect_in_inter` are hand-rolled loops, not
`EC_TXUPD`-traced. So I replayed the COLUMN instead, walking the mi map upward
from the reader (`BlkCell::org` / `dim` / `skip` + `inter_grid` + the deblock
grid), at the exact site:

```text
EC_TXCTXCOL --- column 110 walk, above_txfm now 16
row=79 blk org=(78, 110) dim_mi=(2, 2) blk_px=(8,8)  skip=false inter=false tx_w_grid=4 mode=None
row=77 blk org=(76, 110) dim_mi=(2, 2) blk_px=(8,8)  skip=false inter=false tx_w_grid=4 mode=None
row=75 blk org=(72, 108) dim_mi=(4, 4) blk_px=(16,16) skip=true  inter=true  tx_w_grid=16
row=71 blk org=(64, 104) dim_mi=(8, 8) blk_px=(32,32) skip=true  inter=true  tx_w_grid=32
row=63 blk org=(56, 104) dim_mi=(8, 8) blk_px=(32,32) skip=true  inter=true  tx_w_grid=32
row=55 blk org=(54, 110) dim_mi=(2, 2) blk_px=(8,8)  skip=false inter=false tx_w_grid=4
…
row=39 blk org=(36, 108) dim_mi=(4, 4) blk_px=(16,16) skip=true  inter=true  tx_w_grid=16
```

Two families abutting column 110:

* **skipped inter blocks** — these DO publish, `set_txfm_ctxs`' `skip && is_inter`
  arm, and they put 16 / 32 into the band;
* **8x8 INTRA blocks** (`inter=false`, `tx_w_grid=4`) — these publish NOTHING.

The last writer is therefore the 8x8 intra block at **mi(78,110)**, rows 78–79,
which is exactly the cell libaom's `above_mbmi` names at mi(80,110). Tracing the
decode body that owns it pins the writer:

```text
EC_TXCTXCOL lossless-early-return read_block_tx_size at=(78, 110) side=8 is_inter=false skip=false
```

`side=8`, `is_inter=false`, and the block's own `tx_px` is 4 — a **lossless
segment**, taking `read_block_tx_size`'s lossless early return.

## 3. The cause, in libaom's own words

`av1/decoder/decodeframe.c` (the shared oracle's source, `~/.cache/aom-oracle/src`):

```c
1210 static TX_SIZE read_tx_size(const MACROBLOCKD *const xd, TX_MODE tx_mode,
1211                             int is_inter, int allow_select_inter, aom_reader *r) {
1214   if (xd->lossless[xd->mi[0]->segment_id]) return TX_4X4;     // <-- the early return
1216   if (block_signals_txsize(bsize)) { ... read_selected_tx_size ... }
…
1251   if (cm->features.tx_mode == TX_MODE_SELECT && block_signals_txsize(bsize) &&
1252       !mbmi->skip_txfm && inter_block_tx && !xd->lossless[mbmi->segment_id]) {
1253     … read_tx_size_vartx(…)                                 // var-tx branch
1258   } else {
1259     mbmi->tx_size = read_tx_size(xd, cm->features.tx_mode, inter_block_tx,
1260                                  !mbmi->skip_txfm, r);
1260     set_txfm_ctxs(mbmi->tx_size, xd->width, xd->height,
1261                   mbmi->skip_txfm && is_inter_block(mbmi), xd);   // <-- ALWAYS runs
1262   }
```

A lossless block cannot take the var-tx branch, so it always takes the `else`
branch — and that branch's `set_txfm_ctxs` runs for a lossless block too, with
`tx_size == TX_4X4`, stamping `tx_size_wide[TX_4X4] == 4` over the block's own
width/height (and the BLOCK size instead when `skip_txfm && is_inter_block`).

This decoder's mirror of that `else` branch is `read_block_tx_size`, and its
lossless early return was:

```rust
    if lossless(fctx) {
        … build the TX_4X4 leaf grid, clip it at the frame edge …
        return Ok((4, Some(leaves)));       // <-- published NOTHING
    }
```

The RECT twin `read_block_tx_size_rect` has published on the same arm since
lane-av1lm444loss (`txfm_partition_update_rect(n, at_mi, (4, 4), (bw, bh))`).
**Only the square arm had the hole** — which is why the class was never swept:
the two arms look like twins in the source and only one is.

**Why the reader is the FIRST thing to notice.** Nothing inside the lossless
region reads either band: `read_tx_size` returns before `get_tx_size_context`,
and the var-tx tree is gated on `!xd->lossless`. So every lossless block in a
column can leave the band stale for free, and the first block that *reads* it is
the first LOSSY block below the lossless run. At mi(80,110) that is the 8x4
intra leaf with `use_filter_intra`: `above_txfm[110] = 16 >= 8` gave
`above = 1` and `tx_size_cat0[2]`, where libaom's own `EC_TXCTXB` prints
`abv=4 above=0 left=1 ctx=1`. `left` already agreed (8 vs 8) — the entire gap is
the above TERM off the band, exactly as `lane-av1offtile` §6b measured.

## 4. The fix

`crates/ec-av1/src/decode.rs`, `read_block_tx_size`, lossless arm — one call, the
same one the non-lossless tail already makes at that site:

```diff
     if lossless(fctx) {
         … the TX_4X4 leaf grid, frame-edge clipped …
+        let pub_above: u8 = if skip && is_inter { (side_mi * MI) as u8 } else { 4 };
+        let overwrote = !fctx.intra_only.with(std::cell::Cell::get)
+            && (0..side_mi).any(|i| n.above_txfm.get(at_mi.1 + i) != Some(&pub_above));
+        set_txfm_ctxs(n, at_mi, 4, side_mi, side_mi, skip && is_inter);
+        if overwrote {
+            hit!(LOSSLESS_SQ_TXFM_BAND_OVERWRITE_HITS);
+        }
         return Ok((4, Some(leaves)));
     }
```

`side_mi` is the block's own `side / MI`, matching the extent the non-lossless
tail of the same function already publishes with (`set_txfm_ctxs(n, at_mi, tx,
side_mi, side_mi, skip && is_inter)`), and the `skip && is_inter` term is
libaom's own fourth argument.

**Route counter `LOSSLESS_SQ_TXFM_BAND_OVERWRITE_HITS`**, on the DECISION, not
on function entry: it samples the band's pre-state and fires only when this
publish actually OVERWRITES a size some earlier block of the same frame left in
one of the columns the block covers. It is zero on an intra-only frame (where
`fill_lf_grid_rect_inner` already publishes the same 4x4 value at the same
footprint) and zero on a wholly-lossless stream whose inter blocks are all
non-skip (nothing reads either band there), so it cannot be armed by merely
decoding a lossless fixture.

## 5. Proof

### 5.1 The cell decodes, and is byte-exact

```text
$ EC_AV1_FINAL_DUMP=…/ours/f  decode_probe 420_mixll_altref_640x480_5f.obu
OK: 5 frames decoded, 640x480
frame 0..5: 0 differing bytes   (decode-order dumps vs the oracle's EC_AV1_FINAL_DUMP, 460 800 B each)
```

6/6 for the 5-frame cell and 7/7 for the 6-frame cell (`--limit=6`,
115 615 B), every plane of every decode-order frame including the hidden
alt-ref picture.

### 5.2 The whole-stream ladder, not just up to the fork

Paired `EC_SYMR=1` on both sides, comparing the convention-free fields
(`pre[0]`, `pre[1]`, `n`, `s`, `post_rng`; `pre[2]` carries a per-site constant
offset and `cdf0` the ICDF mirror, so neither is signal), with the third field's
regex accepting a NEGATIVE `pre[2]`:

```text
oracle 457 907 paired reads (0 unparsed)   ours 457 907 paired reads (0 unparsed)
paired ladder bit-identical over ALL 457 907 reads
```

This is the same count the inherited forced-`ctx=1` override reached — the
override was a symptom mask; the publish is the cause and it reaches the same
place without masking anything.

### 5.3 Gate, fixture, mutation

* fixture `crates/ec-av1/fixtures/420_mixll_altref_640x480_5f.obu`, 87 537 B,
  pinned by **fnv1a64** `12707865649077681891` (not a `const SHA256` compared
  against itself), with `420_mixll_altref_640x480_6f.obu` (115 615 B, fnv1a64
  `6783292541028661090`) committed alongside it as the same recipe at `--limit=6`;
* gate `a_420_mixed_lossless_alt_ref_640x480_txfm_band_publish_is_byte_exact_in_decode_order`
  asserts 6 decode-order dumps with 1 hidden, the counter delta `> 0`, and the
  shown-frame per-plane diff against `aomdec --rawvideo`.

```text
$ cargo test -p ec-av1 --lib -- a_420_mixed_lossless_alt_ref_640x480_txfm_band_publish
running 1 test
test stream::tests::a_420_mixed_lossless_alt_ref_640x480_txfm_band_publish_is_byte_exact_in_decode_order ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 806 filtered out; finished in 2.01s
```

**Mutation proof** — the one `set_txfm_ctxs` call surgically removed from the
working tree, the gate re-run:

```text
thread 'stream::tests::a_420_mixed_lossless_alt_ref_640x480_txfm_band_publish_is_byte_exact_in_decode_order' panicked at crates/ec-av1/src/stream.rs:11006:33:
a_420_mixed_lossless_alt_ref_640x480_txfm_band_publish_is_byte_exact_in_decode_order:
this decoder refused the stream: unsupported: AV1 tile (a Golomb tail longer than this decoder reads)
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 806 filtered out
```

Restored, both mixll gates are green together:

```text
$ cargo test -p ec-av1 --lib -- a_420_mixed_lossless_alt_ref_640x480_txfm_band_publish a_420_mixed_lossless_alt_ref_sub8_vartx
running 2 tests
test stream::tests::a_420_mixed_lossless_alt_ref_640x480_txfm_band_publish_is_byte_exact_in_decode_order ... ok
test stream::tests::a_420_mixed_lossless_alt_ref_sub8_vartx_witness_is_byte_exact_in_decode_order ... ok
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 805 filtered out
```

## 6. Corpus sweep — 110 committed fixtures, before and after

Every `crates/ec-av1/fixtures/*.obu` EXCEPT the two this lane adds, decode-order
dumps (`EC_AV1_FINAL_DUMP`) vs the oracle's own `EC_AV1_FINAL_DUMP`, both sides
bit-depth-correct and hidden alt-ref frames included.

| | base `0bfee517` | with the fix |
|---|---:|---:|
| EXACT | 100 | **100** |
| RED (decodes, some frame differs) | 0 | **0** |
| FRAMECOUNT mismatch | 0 | **0** |
| REFUSED | 10 | **10** |

**Per-fixture diff: ZERO rows change.** The two sweeps were taken on this same
worktree with the fix stashed and `git stash pop`-ed (so they differ by exactly
this commit and by nothing else), and a row-by-row comparison over all 110
fixtures agrees on every one:

```text
fixtures: 110   changed rows: 0
```

That is the strongest possible regression statement available here: at
`0bfee517` the committed corpus is already 100 EXACT / 10 REFUSED / 0 RED, and
the publish this lane adds changes nothing it was getting right. The gain is
confined to the two 640x480 fixtures this lane adds, which go

```text
420_mixll_altref_640x480_5f.obu   REFUSED (a Golomb tail longer than this decoder reads)
420_mixll_altref_640x480_6f.obu   REFUSED (the same)
```

Both refusals reproduced on the stashed binary before the fix (§5.1).

All ten refusals are the pre-existing by-name 4:2:2 chroma refusal
(`422_allskip_2f`, `422_intrabc_sb128_strip{,_notxsearch}`,
`422_residual_compound_warp{,_nolr}_16f`, `422_sb128_3f`, `440_request_is_422`,
`W_intrabc`, `X_intrabc_tiled`, `Y_intrabc_10b`) — unchanged, and out of scope
by the project's 4:2:2 decision.

### 6.1 The recipe's already-closed cells

| cell | measured with the fix, on this tip |
|---|---|
| `420_mixll_altref_256x128_5f.obu` (the pin) | **6/6 decode-order EXACT**; its gate green in the same run as the new one (§5.3); its 96997/96997 bit-identical ladder is not re-run here (see §8) — the fix adds no publish that this cell can observe, which the zero-row corpus diff and the re-measured 6/6 both show |
| 176x144 of the same recipe, regenerated and re-encoded here | **6/6 decode-order EXACT** (oracle 6 dumps, 0 differing bytes) |
| 352x288 of the same recipe, regenerated and re-encoded here | **6/6 decode-order EXACT** (oracle 6 dumps, 0 differing bytes) |
| the 4:4:4 arm | green; every `440_*` fixture in the §6 sweep is `EXACT` except the one by-name 4:2:2 refusal |
| `m_320x240`, `m5_320x240`, the 6-frame `m_256x128` residuals named by `lane-av1mixllconj` §7 | **not claimed closed, and not measured here** — they are generated cells, not committed fixtures, so the §6 sweep does not cover them and no number in this report speaks to them |

## 7. Class sweep — `context-band-not-published`

The class: **a decode body that returns with a coded block in hand and leaves
the `TXFM_CONTEXT` band holding a size the block itself should have published.**
Every arm that can return from the two `TXFM_CONTEXT`-publishing builders, and
every site the umbrella gate in `fill_lf_grid_rect_inner` delegates to, re-derived
against libaom's own `parse_decode_block` either/or. Verdicts are by
measurement or by construction, and say which:

| # | site | verdict |
|---|---|---|
| 1 | `read_block_tx_size`, **lossless early return** | **WAS this defect. FIXED** — the 640x480 cell, §3–§5. |
| 2 | `read_block_tx_size`, `!tx_select_inter` early return (returns `(side, None)` / the 128 root's 64x64 grid) | **unobservable by construction.** It fires only when the frame's `tx_mode != TX_MODE_SELECT`, and BOTH band readers are gated on `tx_mode == TX_MODE_SELECT` in libaom: `read_selected_tx_size` is the only caller of `get_tx_size_context`, and `read_tx_size_vartx` is the only caller of `txfm_partition_context` (`decodeframe.c:1251`, `:1193`). Neither runs on such a frame, so the missing publish has no reader. Not fixed, deliberately — see §9. |
| 3 | `read_block_tx_size_rect`, lossless arm (`txfm_partition_update_rect(n, at_mi, (4,4), (bw,bh))`) | **A real libaom-fidelity gap of the same class that IS reached — and has NO observable effect anywhere I measured.** libaom's `set_txfm_ctxs` carries `mbmi->skip_txfm && is_inter_block(mbmi)`, and every caller of this function is an INTER or INTRABC rect block (`is_inter_block` true, `blockd.h:373`), so a SKIPPED lossless rect strip must publish `(bw, bh)` — the block size — where this arm publishes `(4, 4)`. The non-lossless `skip` arm of the SAME function two lines below already gets this right (`txfm_partition_update_rect(n, at_mi, (bw, bh), (bw, bh))`), which is what makes the lossless arm's asymmetry a defect and not a choice. Reached 29 / 1 / 9 / 6 / 16 times across five fixtures (§7.1) and correcting it changed **nothing** on any of them. Handed over, not shipped: with no red-before it is a fidelity edit, not a fix. |
| 4 | `decode_leaf8`'s `if allow_intrabc { set_txfm_ctxs(…) }` (both arms) | **not this class — cleared by measurement.** Instrumenting the `!allow_intrabc` entries: `420_lossless_arf_1to4_320x240_5f` has 140 and the new 640x480 cell 226, and **every one of them is on an INTRA-ONLY frame** (`intra_only=true`, zero on an inter frame) — where `fill_lf_grid`'s `intra_only` umbrella already publishes the same value at the same footprint. The inter-frame 8x8 INTRA leaf is not `decode_leaf8` at all: it is `read_block_tx_size`'s site, i.e. site 1. Left alone. |
| 5 | `fill_lf_grid_rect_inner`'s `publish_txfm_bands && intra_only` umbrella | **not a hole, the delegation itself** — on an inter frame each body must publish at its own site (that is exactly where sites 1 and 3 live). |

### 7.1 Sites 3 and 4, measured

Two temporary `EC_CLASS_SWEEP` rungs (one on `read_block_tx_size_rect`'s
lossless arm, one on `decode_leaf8`'s `!allow_intrabc` entry), run over all 110
committed fixtures and both new 640x480 cells, then removed:

```text
fixture                                    rect_lossless_skip   leaf8_!intrabc (of which inter-frame)
420_lossless_arf_1to4_320x240_5f.obu               29              140  (0)
420_lossless_tallinter_8x16.obu                     1              --
420_mixll_altref_256x128_5f.obu                     9              --
420_mixll_altref_640x480_5f.obu                     6              226  (0)
420_mixll_altref_640x480_6f.obu                    16              --
```

Then the `skip` term of the rect lossless arm was patched on (publish
`(bw, bh)` when `skip`, nothing else changed) and every fixture that reaches it
re-run against the oracle:

```text
SITE3-PATCHED 420_lossless_arf_1to4_320x240_5f: oracle=6  diffbytes=0
SITE3-PATCHED 420_lossless_tallinter_8x16:      oracle=24 diffbytes=0
SITE3-PATCHED 420_mixll_altref_256x128_5f:      oracle=6  diffbytes=0
SITE3-PATCHED 420_mixll_altref_640x480_5f:      oracle=6  diffbytes=0
SITE3-PATCHED 420_mixll_altref_640x480_6f:      oracle=7  diffbytes=0
```

**Site 3 is reached and correcting it is a no-op on every fixture that reaches
 it** — so the divergence has no reader in the whole measured corpus, and it is
 named and handed over rather than shipped without a red-before. The patch was
 reverted; `git diff --stat` for this lane is the publish + counter + accessor +
 gate and nothing else.

## 8. What is NOT measured

* **Site 3 (§7) is not fixed and not gated.** It is a divergence derived from
  libaom's `set_txfm_ctxs` argument and MEASURED to be reached (29/1/9/6/16 hits
  across five fixtures), and correcting it measured as a no-op on every one of
  them. What I did NOT do is build the cell that would make it red, so "no
  fixture has a reader for it" is a measurement over what exists, not a proof
  that no such cell exists.
* **The HBD arm of the class sweep is not synthesized.** No >8-bit y4m was
  produced here; the committed HBD fixtures are covered by the §6 regression,
  which is a regression, not a sweep of this class at 10/12-bit.
* **The 6-frame 640x480 cell has a committed fixture but no gate.** It is
  measured byte-exact (§5.1) and pinned by fnv1a64 in the report, but the gate
  only asserts the 5-frame cell; the 6-frame one is a corpus row, not a gate.
* **Content outside `testsrc2`.** The class is content- and partition-dependent;
  everything above is the one recipe at one size, plus the 110-fixture corpus as
  regression evidence.
* **No `EC_TXCTXCOL` / `EC_TXUPD` / `EC_TXCTX` diagnostic, the forced-`ctx`
  override, or any probe bypass is in the tree.** The two temporary rungs this
  lane added (the column walk and the lossless-early-return tag) were removed
  before the commit; the pre-existing `EC_TXUPD` / `EC_TXCTX` / `EC_TXGRID_TRACE`
  rungs were left exactly as found. The shared oracle was never rebuilt or
  edited.

## 9. Fix-now | deferred(<unblock>) | accepted

* **fix-now** — the one-call publish, its route counter and accessor, the two
  fixtures and the gate. Landed in this commit.
* **fix-now** — the class sweep (§7), because it is what found the site.
* **deferred(unblock: a cell where a SKIPPED inter rect strip sits on a LOSSLESS
  segment AND a lossy block later reads its column — the five fixtures that
  reach §7 site 3 have no such reader, which is why correcting it moved nothing
  — then the `skip` term lands)** — `read_block_tx_size_rect`'s lossless arm is a
  real `context-band-not-published` divergence against libaom, one line wide,
  deliberately not shipped without its red.
* **accepted** — §7 site 2's missing publish on a `tx_mode != TX_MODE_SELECT`
  frame. Unreachable by construction (both readers are gated on SELECT); closing
  it would add a write nothing reads.
* **accepted** — the §6 residuals `lane-av1mixllconj` §7 names on the 6-frame
  256x128 and the 320x240 arms. Not this class, not measured here, not claimed.

## 10. Scoped test command and output

```text
CARGO_TARGET_DIR=$HOME/.cache/cargo-target-av1txctxband2 EC_NOMEMGUARD=1 \
  cargo test -p ec-av1 --lib -- a_420_mixed_lossless_alt_ref_640x480_txfm_band_publish \
                                  a_420_mixed_lossless_alt_ref_sub8_vartx
```

```text
running 2 tests
test stream::tests::a_420_mixed_lossless_alt_ref_640x480_txfm_band_publish_is_byte_exact_in_decode_order ... ok
test stream::tests::a_420_mixed_lossless_alt_ref_sub8_vartx_witness_is_byte_exact_in_decode_order ... ok
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 805 filtered out; finished in 2.06s
```

The corpus sweep (§6) is the regression evidence for everything else: 110
fixtures swept before and after with **zero rows changing**.

## 11. Hunk scope

* Authored here: `crates/ec-av1/src/decode.rs` (the publish + counter + accessor),
  `crates/ec-av1/src/stream.rs` (the gate), two fixtures under
  `crates/ec-av1/fixtures/`, and this report.
* No line outside those hunks was touched. The primary checkout was never
  edited — one `edit` call leaked into it early and was reverted with
  `git checkout --` before any build; `git status --porcelain` there is clean.
* The shared oracle at `~/.cache/aom-oracle` was read only.
