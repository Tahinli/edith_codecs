# lane-av1sb128fork — the 4:4:4 lossy `--sb-size=128` entropy fork is a `get_tx_size_context` leak, not a var-tx leaf-list difference

Base / branch: `8680e31d` (main) → `lane-av1sb128fork`, worktree
`~/.cache/wt/av1sb128fork`. **No decode-path change is left on this branch** —
`git status --porcelain` in the worktree is empty and the main checkout is
empty. The report is the deliverable. Every number below was measured in this
worktree on the reverted (clean) tree unless a line says it is quoted.

Build: `CARGO_TARGET_DIR=/home/tahinli/.cache/tgt-av1sb128fork` (private).
`cargo check -p ec-av1 --all-targets`: clean, 0 warnings.
Oracle: `~/.cache/aom-oracle/build/{aomenc,aomdec}` (instrumented), read-only,
run with `--threads=1` (the rung order is identical with and without it).

---

## 0. Verdict first

The fork **reproduces** and is a **CDF-row (context) error on a 2-symbol
`tx_size_cdf` read**, not the transform-unit-enumeration difference the ticket
hypothesised. Measured, in this order:

1. The var-tx trees **agree exactly** in the frame that forks (§3).
2. The partition tree **agrees exactly** in the SB that forks (§4).
3. The single differing entry is the **tx-size context of the 8x4 intra leaf at
   mi(57,52)**: `get_tx_size_context` returns **2** in libaom and **1** here,
   because our `above_inter` mi-band is `true` at that cell with
   `above_side_mi = 4` while libaom's `is_inter_block(above_mbmi)` is `false`
   there (§5).

So `tx_size_cat0[1]` is read where libaom reads `tx_size_cat0[2]`, and the coder
forks on the very next symbol.

---

## 1. The witness

```text
ffmpeg -f lavfi -i "testsrc2=size=256x256:rate=25" -frames:v 2 \
       -pix_fmt yuv444p -strict -1 -f yuv4mpegpipe -y src.y4m
aomenc --codec=av1 --passes=1 --end-usage=q --cq-level=62 --cpu-used=2 \
       --threads=1 --row-mt=0 --lag-in-frames=0 --kf-max-dist=100 \
       --sb-size=128 --profile=1 --obu -o t256.obu src.y4m
```

| artifact | value |
|---|---|
| OBU | `t256_n2_cq62.obu`, 6186 B, sha256 `14acb74e825a2bb3ef195a52cc2abc67794d9c4d92080b66c739d318177fc7c6` |
| geometry | 256x256, 4:4:4 (`--profile=1`, `subsampling 0/0`), 8-bit, 2 frames, `--sb-size=128` (2x2 superblocks) |
| encode | 1.3 s on this box; the idle VPS was not needed |

Pixel impact vs `aomdec` (`EC_AV1_FINAL_DUMP` vs `dump_yuv`, per plane):

| frame | Y | U | V | of |
|---|---|---|---|---|
| 0 (key) | 841 | 1295 | 1372 | 65536 |
| 1 (inter) | 56986 | 55974 | 56442 | 65536 |

## 2. The lockstep divergence, quoted

`EC_SYMR=1` on both sides, aligned on `(value, range, n, s, post_rng)` with the
CDF-row convention reconciled (`a == b or 32768 - a == b`; that predicate holds
on **every** paired read before the fork, and after it except where the ranges
have already separated). Aligner: `~/.cache/sb128fork/align.py`.

```
ours 41245 reads | oracle 37229 reads
*** FORK at read index 32813 ***
[32812] ours  pre=(4601,35120,45417) cdf0=2586  n=14 s=12 post_rng=45492  (cdf=uv_mode)
        aom   pre=(4601,35120,45402) cdf0=30182 n=14 s=12 post_rng=45492  (decodemv.c:146, read_intra_mode_uv)
<<32813>> ours  pre=(7550,45492,45418) cdf0=6424  n=2 s=1 post_rng=36377
         aom   pre=(7550,45492,45403) cdf0=19938 n=2 s=1 post_rng=55054  (decodeframe.c:1106, read_selected_tx_size)
<<32814>> ours  pre=(7550,36377,45418) cdf0=6506  n=2 s=1 post_rng=58228
         aom   pre=(15100,55054,45404) cdf0=26262 n=2 s=1 post_rng=44079 (decodetxb.c:158)
```

- Read index **32813** (0-based; file line 32814). **32812** reads agree on
  `(value, range, n, s, post_rng)` and on the reconciled CDF row.
- The two sides hold the **identical pre-state** `pre=(7550, 45492)` and the
  **identical symbol width** `n=2` and the **identical symbol value** `s=1`.
  Only the CDF row differs: ours `cdf0=6424` (`32768-6424 = 26384`) vs the
  oracle's `19938`. `26384 != 19938`, so this is **not** the mirror convention —
  it is a genuinely different row of the same table.
- The bit-position third field differs by the documented constant `+15`
  (45418 vs 45403), i.e. the coder state is in sync. **This is a context error,
  not a bit-position error.**
- The mi label `(57,52)` is the SYMR block label on both sides. The real block
  identity is in §5.
- `decodeframe.c:1106` is `read_selected_tx_size`:
  `aom_read_symbol(r, ec_ctx->tx_size_cdf[bsize_to_tx_size_cat(bsize)][get_tx_size_context(xd)], bsize_to_max_depth(bsize)+1)`.
  `n = max_depths+1 = 2`.

**The same signature reproduces on 12 further cells**, all with the *same* oracle
site `decodeframe.c:1106` and all `n=2` on both sides (a `WIDTH-MISMATCH`-free
table; `hunt4.py`, every row is `same-width`):

| cell | reads ours/oracle | fork index | ours `(n,s,cdf0)` | oracle `(n,s,cdf0)` |
|---|---|---|---|---|
| 256x256 cq54 | 59780 / 53575 | 34496 | (2,1,5938) | (2,1,18898) |
| 256x256 cq56 | 49707 / 40068 | 34153 | (2,1,5100) | (2,1,24247) |
| 256x256 cq60 | 48400 / 38531 | 33639 | (2,1,5817) | (2,1,24434) |
| 256x256 cq62 (2f) | 41245 / 37229 | 32813 | (2,1,6424) | (2,1,19938) |
| 256x256 cq62 (4f) | 53195 / 61630 | 32813 | (2,1,6424) | (2,1,19938) |
| 320x240 cq36 | 100146 / 56168 | 39272 | (2,1,6025) | (2,1,22369) |
| 320x240 cq40 | 60107 / 51406 | 36092 | (2,1,4212) | (2,1,18729) |
| 320x240 cq48 | 67609 / 45480 | 33001 | (2,1,7518) | (2,1,23633) |
| 320x240 cq52 | 47529 / 43753 | 31700 | (2,1,5996) | (2,1,21916) |
| 320x240 cq62 | 54627 / 38560 | 31107 | (2,1,6120) | (2,1,21114) |
| 256x384 cq30 | 119234 / 81998 | 51581 | (2,1,2485) | (2,1,21113) |
| 256x384 cq36 | 93732 / 71009 | 48493 | (2,1,3446) | (2,1,20649) |

Every fork in the class is the **same 2-symbol intra `tx_size_cdf` read with the
wrong row**. I did not find a cell in this family where the two sides read
*different widths* at the fork, so I could not reproduce the ticket's
`oracle n=7 / ours n=6` observation; see §7.

**The fork is in frame 0, the KEY frame** (frame 0 spans SYMR reads 0..33894;
the boundary is the `bitpos` reset 45488 -> 15 at line 33896). The ticket calls
the recorded cell an INTER-frame fork; on every cell I measured the fork is in
the key frame of a 2+ frame stream. §7 says what that does and does not decide.

---

## 3. Measurement (1) — the var-tx leaf-list diff, and why the hypothesis dies

The ticket ranked "diff both sides' var-tx leaf lists for the SB containing
mi(56,48)" first. I mirrored the oracle's `EC_VARTX` rung
(`read_tx_size_vartx`, `decodeframe.c:1060`) inside our `read_var_tx_size` and
added a per-block leaf-list dump at the tail of `read_block_tx_size`'s inter
branch (both temporary, both reverted). Then:

**Whole-stream split-read counts: ours 89, oracle 12.** That looks like the
ticket's "548 vs 1314" shape — and it is entirely an artefact.

**In frame 0, the frame that forks, there is exactly ONE `txfm_partition` split
symbol on each side, and the two are the same read:**

| side | line |
|---|---|
| oracle | `EC_VARTX mi=(56,52) row=0 col=0 ctx=19 n=2 s=0 bitpos=45312` |
| ours | `EC_VARTX mi=(56,52) row=0 col=0 ctx=19 n=2 s=0 bitpos=45326 rng=48008` |

Same mi, same `ctx=19`, same value `s=0`, `bitpos` differing by the same +14 as
every other paired read. **The var-tx trees in the forking frame are identical,
so there is no missing/extra var-tx leaf and no leaf whose size differs.** The
89-vs-12 gap is 88 reads in frame 1, which is *downstream* of the fork: once the
coder desyncs, every later partition/split read is garbage. The ticket's
"structural difference in transform-unit enumeration" is a **consequence** of
the fork, measured on the wrong side of it.

For completeness, the one block on that side whose leaf list is a list of more
than one leaf (our per-block dump, frame 0 has no multi-leaf var-tx block; this
is frame 1's 128 root, quoted only to show the dump works and is post-fork):

```
EC_VLEAFLIST mi=(0,32) side=128 max_tx=64
  leaves=[(0,0,64,64), (0,16,64,64), (16,0,16,16), (16,4,16,16), (20,0,16,16),
          (20,4,16,16), (16,8,32,32), (24,0,32,32), (24,8,32,32), (16,16,64,64)]
```

with the matching split reads

```
EC_VARTX mi=(0,32)  row=0  col=0  ctx=1  s=0
EC_VARTX mi=(0,48)  row=0  col=16 ctx=0  s=0
EC_VARTX mi=(16,32) row=16 col=0  ctx=1  s=1
EC_VARTX mi=(16,32) row=16 col=0  ctx=4  s=1
EC_VARTX mi=(16,40) row=16 col=8  ctx=4  s=0
EC_VARTX mi=(24,32) row=24 col=0  ctx=5  s=0
EC_VARTX mi=(24,40) row=24 col=8  ctx=3  s=0
EC_VARTX mi=(16,48) row=16 col=16 ctx=1  s=0
```

Post-fork, so **not** evidence for anything; recorded only so the next reader
does not re-derive it.

---

## 4. Measurement (2) — the `EC_PART` trees for SB(1,1), frame 0

The oracle's `EC_PART`/`EC_PART_VAL` and our twin, filtered to mi rows/cols
`32..63` (SB row 1, col 1) and frame 0. **Every paired read agrees on
`(mi, bsize, value)`; the only mismatch is the first read after the fork.**

| # | oracle | ours |
|---|---|---|
| 1 | `mi=48,48 bsize=12 ctx=15 value=3` | `mi=48,48 bsize=12 ctx=3 value=3` ✓ |
| 2 | `mi=48,48 bsize=9 ctx=11 value=3` | `mi=48,48 bsize=9 ctx=3 value=3` ✓ |
| 3 | `mi=48,48 bsize=6 ctx=4 value=0` | `value=0` ✓ |
| 4 | `mi=48,52 bsize=6 ctx=5 value=9` | `value=9` ✓ |
| 5 | `mi=52,48 bsize=6 ctx=4 value=0` | `value=0` ✓ |
| 6 | `mi=52,52 bsize=6 ctx=5 value=9` | `value=9` ✓ |
| 7 | `mi=48,56 bsize=9 ctx=11 value=6` | `mi=48,56 bsize=9 ctx=3 value=6` ✓ |
| 8 | `mi=56,48 bsize=9 ctx=11 value=3` | `mi=56,48 bsize=9 ctx=3 value=3` ✓ |
| 9 | `mi=56,48 bsize=6 ctx=4 value=0` | `value=0` ✓ |
| 10 | `mi=56,52 bsize=6 ctx=5 value=3` | `value=3` ✓ |
| 11 | `mi=56,52 bsize=3 ctx=1 value=1` | `mi=56,52 bsize=3 ctx=1 value=1` ✓ |
| **12** | `mi=56,54 bsize=3 ctx=3 value=2` | `mi=56,54 bsize=3 ctx=3 value=0` ✗ |

Row 12 is the first value divergence and it sits **after** the fork read
(oracle `tell=45410`, ours `tell=45442`; the fork is at 45403/45418) — i.e. it is
damage, not cause. The `ctx` numbers are not comparable across sides because our
`Cdfs` keeps one `partition_cdf` array per size group (`partition_w64/w32/w16/w8`)
while libaom keeps one flat `partition_cdf[20]`; adding `4*bsl`
(`PARTITION_PLOFFSET`) to ours reproduces the oracle's ctx on **all 11** agreed
rows, so the partition context is right too.

**One caveat I am recording rather than fixing:** our `EC_PART` rung misprints
`mi` on one of its four arms. Rows 3-6 and 9-10 print `mi_row=12/13/14,
mi_col=12/13` where the block is at `mi=48/52/56, mi_col=48/52` — the printed
value is the 16-px-unit index (`mi/4`) on that arm only. The `value=`, `rng=`
and read ORDER are correct, which is why the pairing above still holds. That is a
diagnostic-rung defect in `decode.rs`'s `EC_PART` arms, not a decode defect, and
it is **not** this lane's fix.

---

## 5. The actual differing entry: the tx-size context of the 8x4 leaf at mi(57,52)

The oracle's `EC_TXCTXB` rung (`get_tx_size_context`, `pred_common.h:348`) and
our `EC_TXCTX` (`tx_size_context_txfm_rect`, `decode.rs:28114`) line up
**row for row** from index 0 to the fork — 300+ rows, `own`/`maxw`, `abv`/
`above_txfm`, `lft`/`left_txfm`, `above`, `left` and `ctx` all equal. The first
divergence is the fork:

```
oracle  EC_TXCTXB mi=57,52 bsize=2 maxw=8 maxh=4 hasup=1 hasleft=1 \
                 abv=8 lft=8 above=1 left=1 ctx=2
ours    EC_TXCTX  mi=57,52 own=8x4 ha=true hl=true intra_only=true \
                 above_txfm=8 left_txfm=8 above_inter=true left_inter=false \
                 above_side=4 left_side=16 above=false left=true
```

- Same block: `bsize=2` is libaom's `BLOCK_8X4`; our own block is `8x4`.
- Same bands: `abv=8` == `above_txfm=8`, `lft=8` == `left_txfm=8`.
- The only difference is `above_inter`: **we have `true`, and `above_side=4`.**
  Our override fires and gives `above = (4 >= 8) = 0`; libaom's
  `is_inter_block(above_mbmi)` is **false** at that cell, so it keeps
  `above = (above_txfm_context[0] >= 8) = 1`.
- Hence `ctx = 0+1 = 1` (ours) vs `1+1 = 2` (libaom), and the read is
  `cdfs.tx_size_cat0[1]` where libaom reads `tx_size_cdf[...][2]`.

I confirmed the table identity on our side rather than inferring it: a temporary
`ptr={:p}` on the `EC_SYMR` line plus a one-shot dump of the four
`tx_size_cat*` base addresses gives, at the fork read,
`ptr=0x…41be` against `tx_size_cat0[0]=0x…41b8` — **row 1 of `tx_size_cat0`**,
a 3-entry (2-symbol) row, exactly the `n=2` the oracle shows. The
instrumentation is reverted.

The next row also differs, and it is the block-shape half of the same story:

```
oracle  EC_TXCTXB mi=56,54 bsize=1 maxw=4 maxh=8 abv=4 lft=4 above=1 left=0 ctx=1
ours    EC_TXCTX  mi=56,54 own=8      above_txfm=4 left_txfm=4 above_inter=false \
                 left_inter=true above_side=4 left_side=4 above=false left=false
```

We resolve an 8x8 block there where libaom has `BLOCK_4X8`; that row is
post-fork (`left=0` still agrees, so it is not the cause).

## 6. The rule, with the libaom cite

`get_tx_size_context` — `av1/common/pred_common.h:348-380`:

```c
const TX_SIZE max_tx_size = max_txsize_rect_lookup[mbmi->bsize];
const int max_tx_wide  = tx_size_wide[max_tx_size];
const int max_tx_high  = tx_size_high[max_tx_size];
int above = xd->above_txfm_context[0] >= max_tx_wide;
int left  = xd->left_txfm_context[0]  >= max_tx_high;
if (has_above) if (is_inter_block(above_mbmi)) above = block_size_wide[above_mbmi->bsize]  >= max_tx_wide;
if (has_left)  if (is_inter_block(left_mbmi))  left  = block_size_high[left_mbmi->bsize]   >= max_tx_high;
return (has_above && has_left) ? (above + left) : (has_above ? above : (has_left ? left : 0));
```

The override is gated on `is_inter_block(xd->above_mbmi)` — a property of **the
one `MB_MODE_INFO` at `xd->mi[-mi_stride]`**, i.e. of the block whose bottom
edge abuts this block's top edge at this column. `is_inter_block`
(`blockd.h:373`) is `is_intrabc_block(mbmi) || ref_frame[0] > INTRA_FRAME`.

Our `tx_size_context_txfm_rect` (`decode.rs:28114-28164`) reads the same four
bands, but `above_inter` / `left_inter` are **mi-granular bands that any intrabc
block stamps `true` for its own column span** (`record_intrabc_mi_rect`,
`decode.rs:4905-4925`: `for i in 0..n4_w { above_inter[mi_c + i] = true }`),
and cleared to `false` only by `Neighbours::fill_lf_grid_rect`'s intra-only
stamp (`decode.rs:9384-9418`). So the value at a cell is whatever the **last
block touching that column** left, not "is the block immediately above inter".
At mi row 56, col 52 that value is `true` with `above_side_mi = 4`; libaom's
`above_mbmi` there is intra.

This is the class the crate's own doc comments already name twice, on
neighbouring code: `tx_size_context_txfm_rect`'s header ("an *inter* neighbour
contributes its own BLOCK size rather than its transform size … which is what the
`above_inter`/`left_inter` bands carry") and `lane-intrasplit r3` at
`decode.rs:28166-28171` ("`above_inter`/`left_inter` are mi-granular bands …
the inter-neighbour override fired on the wrong neighbour"). The current instance
is the **stale-band** half of that, not the granularity half.

## 7. What I could NOT decide, and the one measurement that would

- **I did not reproduce the ticket's exact cell.** I never found a cell whose
  fork is `oracle n=7 / ours n=6` (the ticket's `n=7` is an `eob_flag_cdf64`
  width and `n=6` an `eob_flag_cdf32` width, which *would* be a 64-wide vs
  32-wide transform-unit difference). Twelve cells in the same family all fork
  `same-width` on the intra `tx_size_cdf` read instead. So the ticket's claim
  "our width-7 read is not a tx_type read, the walk is at a DIFFERENT UNIT" is
  **not** what I measured: on every cell I measured the two sides are at the
  **same** block reading the **same** 2-symbol table, and the only difference is
  the row. If the ticket's cell is a different defect, this report does not
  cover it; if it is the same class on a different geometry, the fix below
  covers it.
- **The ticket says INTER frame; every fork I measured is in the KEY frame.**
  Frame 1's partition/var-tx divergence on this cell is entirely post-fork
  damage. I could not separate "the recorded cell's stream had a different frame
  layout so the fork landed in an inter frame" from "the recorded cell is a
  different defect".
- **Not decided: which write leaves `above_inter[52] = true` at mi row 56.** The
  two candidates are (a) an intrabc block earlier in column 52 stamped `true`
  and the block that owns mi(56,52) did not clear it, and (b) the block owning
  mi(56,52) is a sub-8 leaf whose `fill_lf_grid_rect` span does not cover
  col 52. I did not add that instrumentation.
- **No fix is shipped.** A fix here is a change to when the mi-granular
  `above_inter`/`left_inter` bands are cleared/stamped, which is the
  `lane-intrasplit r3` / `lane-t900 r1` territory, and I could not measure a
  red-before for it or re-quote the neighbour gates inside this lane. The
  narrowing above is what I stand behind.

**The deciding measurement** (one flag, one cell): print, at the fork read only,
(a) our `above_inter[mi_c]`, `above_side_mi[mi_c]` and the `(mi_r, mi_c, bw, bh,
is_intrabc, ref_frame[0])` of every block that stamps column 52 between the last
`above_inter[52] = false` and this read, and (b) from the oracle the
`above_mbmi->bsize`, `use_intrabc` and `ref_frame[0]` of the block at
`xd->mi[-mi_stride]` for mi(57,52). If (b) says intra and (a) names a stale
intrabc stamp, the fix is "clear `above_inter`/`left_inter` over the block's own
column/row span at every block, not only in the intra-only path, and stamp
`above_side_mi` with the abutting block's own `block_size_wide/high`"; if (b)
says intrabc, the fix is the `above_side_mi` value (4 is not a legal
`block_size_wide` for an intrabc block, whose minimum is `BLOCK_8X8`).

---

## 8. Files and hygiene

- Branch diff vs `8680e31d`: **this report only**.
- Temporary instrumentation added and then fully reverted (`git checkout --
  crates/ec-av1/src/decode.rs crates/ec-av1/src/msac.rs`): the `EC_VARTX`
  mirror + `EC_VLEAF`/`EC_VLEAFLIST` dumps in `read_var_tx_size` /
  `read_block_tx_size`, the `EC_TXSITE`/`EC_TXSITE_RECT`/`EC_TXCATPTRS` dumps at
  four `tx_size_cat*` read sites, and the `ptr={:p}` field on the `EC_SYMR`
  line. `git status --porcelain` is empty in the worktree **and** in the main
  checkout; `cargo check -p ec-av1 --all-targets` is clean afterwards.
- Throwaway scripts and dumps, outside the repo: `~/.cache/sb128fork/`
  (`align.py`, `hunt.sh`, `hunt2.py`, `hunt3.py`, `hunt4.py`, `txctx_align.py`,
  `part_align.py`, the `*.aom.txt` / `*.our.txt` / `*_vartx*.txt` /
  `*_txctx.txt` / `*_part.txt` traces, and the encodes).
- No committed fixture, no committed gate: the class is diagnosed, not fixed, and
  §7 is explicit about what is still undecided.

## 9. Coordination

Declared to `Gamze-4` (lane-av1sb128chroma) before any edit: I touch only the
luma/var-tx/tx-size-context path; the chroma unit loops, `av1_get_max_uv_txsize`
and the 4:4:4 chroma shape code are theirs. Gamze-4 confirmed no collision
(its decode.rs diff is empty, its whole diff is one test + two fixtures + a
report). Its `444_sb128cq20_tsrc2_control.obu` is 128x96 — one superblock row,
so it cannot reach an SB at mi row 32+; that is why I re-encoded at 256x256
here. It also flagged `lanes/av1txsizeaudit.report.md` and
`lanes/av1f9singleland.report.md`; I read the latter (§0 of this report is
consistent with its verdict that the recorded 4:4:4 divergences were already
closed by H1) and grepped the former for this class — its per-line verdicts do
not cover the 4x8/8x4 intra leaf's `above_inter` band.
