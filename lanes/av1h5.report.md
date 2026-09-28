# lane-av1h5 — 4:4:4 lossless 10-bit, 128-root partition: localized, NOT fixed

Handoff of H5 from `lanes/av1formatsweep.report.md`. Worktree
`~/.cache/wt/av1h5`, branch `lane-av1h5`, base `4155c7c7`. **Nothing
committed, nothing pushed** — the budget ran out mid-localization and the
defect is NOT fixed. Everything below is measured, not inferred.

## 1. Witness

```
ffmpeg -y -v error -f lavfi -i testsrc2=s=128x96:r=25 -frames:v 6 \
  -pix_fmt yuv444p10le -strict -1 -f yuv4mpegpipe /tmp/h5.y4m
~/.cache/aom-oracle/build/aomenc --codec=av1 --profile=1 \
  --input-bit-depth=10 --bit-depth=10 --lossless=1 --enable-palette=0 \
  --enable-intrabc=0 --cq-level=20 --cpu-used=0 --passes=1 --end-usage=q \
  --threads=1 --row-mt=0 --lag-in-frames=0 --kf-max-dist=100 --limit=6 \
  --obu -o /tmp/h5.obu /tmp/h5.y4m
```

63429 bytes, **sha256 `bbffb979d20d581deb271aac75b7ee779dde57c024524ed6b1389356f55ca0fa`**,
6 frames, 128x96, `ss (0,0)` at 10-bit, lossless.

## 2. The control arm (this is what makes the rest a decoder verdict)

Same recipe, one flag changed:

| arm | bytes | verdict vs `aomdec --rawvideo` |
|---|---|---|
| `--max-partition-size=64` (no 128 root) | 61494 | **EXACT**, all 6 frames |
| default (128 root reachable) | 63429 | **DIVERGES**, first at byte 74769 |

So the comparison harness is sound on this exact content, and the 128x128
unsplit root is **necessary** for the defect. The sibling at `--cpu-used=2`
(gate B, `a_lossless_444_10bit_inter_stream_decodes_pixel_exact`) never reaches
the 128 root and is exact.

## 3. First divergence

Whole-stream `dump_yuv` (u16 LE) concatenated, compared byte-for-byte with
`aomdec --rawvideo`:

* first differing byte: **74769** of the concatenation
* frame size is 128*96*3*2 = 73728 bytes, so this is **decode-order frame 1**,
  offset 1040 bytes = sample 520
* 520 = row 4, col 8 of the 128-wide luma plane → **first wrong sample Y(4, 8)**

(`av1formatsweep.report.md` recorded this site as `Y(8,4)`; same site, row and
column transposed.)

## 4. EC_SYMR ladder — entropy fork, not reconstruction

Paired `EC_SYMR` traces (ours `msac.rs`, oracle `aom_dsp/bitreader.h`), aligned
on `(range, post_rng, s, n)`; the CDF convention reconciles as
`32768 - ours_icdf0 == oracle_cdf0` (verified over the first 2000 reads).

| index | oracle | ours | |
|---|---|---|---|
| 43834-43839 | `decodetxb.c:158` n=2 s=1, cdf0 32717..32729 | icdf0 51..39, mirror matches | agree |
| **43840** | `decodetxb.c:158` n=2 s=1 **cdf0=32731** | icdf0=63 → mirror **32705** | **first CDF-state divergence** |
| 43841-43842 | cdf0 32732, 32733 | mirror 32708, 32711 | diverging |
| **43843** | `decodetxb.c:242` n=5 **s=3** | **s=4** | **first SYMBOL divergence** |

* `decodetxb.c:158` is the `txb_skip` / `all_zero` read:
  `aom_read_symbol(r, ec_ctx->txb_skip_cdf[txs_ctx][txb_ctx->txb_skip_ctx], 2, ...)`.
* `decodetxb.c:242` is the EOB pass-count read, `eob_flag_cdf16[plane][ctx]`, 5 symbols.
* Both sides at the fork: `ph=inter`, `mi=(0,0)`, identical pre-state
  (`pre=(49549, 65280, …)`, range 65280).

**Verdict: entropy fork.** The decoder selects a different `txb_skip` CDF ROW
for a chroma unit; the symbols happen to survive three reads and then diverge.
Nothing here is reconstruction arithmetic — the two decoders are reading
different probability distributions from the same bit position.


---

## 9. RESOLUTION (lane-av1h5 r2, commit `8a91ee14`)

### 9a. The handoff's suspect site is REFUTED, not fixed

§5 named `read_plane`'s chroma band (`decode.rs:19627-19636`) and pointed at
`luma_skip_ctx`. I instrumented the band selection on our side (a
`EC_DBGCTX`-shaped rung printing `plane`, `tx`, `above0`, `left0`, `base`,
`off`, `ctx`) and diffed it against the oracle's own `EC_DBGCTX` for all
**9216** chroma units of the stream.

**The band is correct at 100% of them.** After the port's `+7` reindex
(our rows 0..2 are libaom's 7..9, our 3..5 are libaom's 10..12):

```
compared=9216  off/ctx diffs=0
```

Frame 0 (1536 units) matches the oracle unit-for-unit. Frames 1–5 are
**100% `off=10`** on both sides — the 520 `off=7` units in the oracle census
are all frame 0 (measured with `--limit=1/2/3`: 1536 / 3072 / 4608). §5's
"at the fork site every chroma unit is tx=0 with off=7" was a grep that
matched frame 0's `mi=0,0` lines, which recur in every frame.

So the offset term was never wrong, and no `+3` was missing. **Do not touch
`read_plane`'s band.** (Its doc comment, which scoped the `+3` to "a 128x128
block's TX_32X32 chroma units", is now corrected: the 4:4:4 lossless 128
root's TX_4X4 chroma units read the offset-10 rows too.)

### 9b. The real defect: the token order at a 128 root is chunk-major

Our chroma stream diverged from the oracle's at unit **1539** with the band
right and `ctx_base` wrong — a left-context that had not been published yet.
Instrumenting the walk that produces it (`read_inter_chroma_lossless`) with
its caller, region and unit geometry:

```
EC_DBGCTX walk n=1536 at_mi=(0,0) org=(0,0)  reg=(64,64) blk=(128,128) true=(128,96)
EC_DBGCTX walk n=2049 at_mi=(0,0) org=(64,0) reg=(64,64) blk=(128,128) true=(128,96)
EC_DBGCTX walk n=2562 at_mi=(0,0) org=(0,0)  reg=(64,64) blk=(128,128) true=(128,96)   <-- again
```

320 walk calls for 5 inter frames — **64 per frame, where libaom codes 4** —
and each `chunkdone` showed `leaves=1024` (the full 32x32 mi block, not the
frame's 32x24) firing every 16 leaves:

```
chunkdone idx=15  chunk=(0,0) at=(0,15,4,4)  next=Some((0,16,4,4))
chunkdone idx=31  chunk=(0,1) at=(0,31,4,4)  next=Some((1,0,4,4))
chunkdone idx=47  chunk=(0,0) at=(1,15,4,4)  next=Some((1,16,4,4))   <-- back to (0,0)
```

Ground truth, `decodeframe.c:972-1006`: `decode_token_recon_block` walks
`for (row…) for (col…) { for (plane…) { for (blk_row…) for (blk_col…) } }` —
the **plane loop is inside the chunk loop**, and `blk_row`/`blk_col` restart
at the chunk origin. The chunk grid is `mu_blocks_wide = mi_size_wide[BLOCK_64X64] = 16` mi
per axis, i.e. **16 mi across, not 32**.

Our var-tx leaf list is a BLOCK-wide raster (32 leaves across at 128), so a
chunk's members are **not contiguous**: leaves 0..15 are chunk (0,0) row 0,
but chunk (0,0) rows 1..15 are leaves 32..47, 64..79, … A chunk test
`(leaves[idx+1].0/16, leaves[idx+1].1/16) != (row/16, col/16)` therefore
closed the chunk every **sixteen** leaves, and each of the four chunks' chroma
was coded sixteen times — 24576 chroma units per inter frame against the
oracle's 1536.

Below 64 a block IS one chunk, so the two orders coincide and every committed
pin passed. **This is the only committed cell where an unsplit BLOCK_128X128
root meets lossless.**

### 9c. The fix and the class sweep

`grep "row / 16, col / 16"` finds **four** sites carrying the test. All four
now sort the leaf walk chunk-major at `side > 64` and take the successor from
that order:

| site | arm |
|---|---|
| `decode.rs:13543` | inter 128x64 / 64x128 (compound/intrabc rect) |
| `decode.rs:39268` | intra 128 |
| `decode.rs:40932` | inter 128x128 (**the H5 cell**) |
| `decode.rs:41952` | inter vartx 128 |

The order key is `(row/16)*nchunk + (col/16)`, ties broken by the original
index, so within a chunk the leaves keep the tree's own raster — which is what
libaom's per-chunk `blk_row`/`blk_col` walk is.

### 9d. Measured, on the pinned fixture vs `aomdec`

| | before | after | oracle |
|---|---|---|---|
| chroma walks / inter frame | 64 | **4** | 4 |
| chroma units / inter frame | 24576 | **1536** | 1536 |
| chroma units, whole stream | 124416 | **9216** | 9216 |
| first `skip_ctx` divergence | unit 1539 | unit 2573 | — |
| first differing pixel byte | 74769 | **90272** | — |
| oracle plane sequence / frame | — | 256,256,256,256,128,128,128,128 ×2 | same |

Frame 0 is unaffected (it splits, no 128 root) and still matches the oracle
exactly on all 1536 chroma units.

### 9e. Verification

* **Identity**: `lossless_444` 6/6, `sb128` 12/12 (1 pre-existing `#[ignore]`),
  `420` 2/2, `vartx` 3/3, `a_real_aomenc_tiny_frame_size_sweep`,
  `a_sweep_of_doubly_straddling_sizes_round_trips_through_ffmpeg` all pass.
* **Control arm**: the same recipe at `--max-partition-size=64` (61494 B,
  sha256 `4cadce41e1f89675…`) decodes **byte-exact** — the 128 root is still
  necessary, and the fix does not touch it.
* **Gate**: `a_lossless_444_128_root_lossless_stream_reads_chunks_chunk_major`,
  pin `fixtures/ll444_128root_lossless.obu` (63429 B, sha256 `bbffb979…`,
  fnv1a64 `0xf07d47fcfd512658`). Non-vacuous: it asserts `mu_chunk_order_hits`
  rose, and pins the first-difference position at `>= 90272`.
* **Mutation (red-before)**: rewriting the chunk key back to a block raster
  (`(col/16)*1000 + c`) turns the gate **RED**; reverting turns it green.

### 9f. NOT fixed — the residual, and what it is not

**The fixture is still not byte-exact.** One defect remains, and it is a
*different* one, below the chunk-order bug this lane fixed:

* first difference: **byte 90272** = decode-order frame 1, sample 4136 →
  **Y(32, 40)** (was Y(4, 8)).
* first chroma `skip_ctx` divergence: global unit **2573** — frame 1, plane 1,
  the clipped lower chunk row.
* It is **not** a band/offset error (off matches at every unit) and **not**
  the `ctx_base`/left-context gather: at unit 2572 both sides read the SAME
  `skip_ctx` (11) and the oracle's unit 2572 publishes level 0 while ours
  publishes non-zero. So the two decoders decode *different coefficients*
  from an identical CDF row at 2572 — the fork is at the coefficient decode,
  not at context selection.

That is the next thing to chase, and it needs its own EC_SYMR pass anchored
at unit 2572. §7's plan is superseded: **the band is fine, the order was not.**

### 9g. Interaction with H1

Untouched and unaffected: `strip_chroma` is still `Some` only from the
PARTITION_HORZ_4 / VERT_4 setter, which an unsplit BLOCK_128X128 root never
enters. This lane's change is the leaf-walk ORDER at `side > 64`; it touches
no `strip_chroma` site and no chroma-context predicate, so it cannot double-fix
Levent-2's arm. Their stream should be unaffected — `agent://Levent-2` was
notified.

### 9h. State

Branch `lane-av1h5`, two commits on top of `4155c7c7`:
`11954074` (report as handed over, incl. §6b) and `8a91ee14` (the fix, the
gate, the pin). Nothing pushed. All instrumentation was env-gated and has been
removed from the commit; no `EC_AV1_ALLOW_422_PROBE` bypass was used or needed.
