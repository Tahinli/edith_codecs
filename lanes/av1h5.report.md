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

## 5. Where the row selection goes wrong (the handoff)

libaom, `get_txb_ctx_general` (`av1/common/txb_common.h:353-359`), chroma arm:

```c
const int ctx_base = get_entropy_context(tx_size, a, l);
const int ctx_offset = (num_pels_log2_lookup[plane_bsize] >
                        num_pels_log2_lookup[txsize_to_bsize[tx_size]]) ? 10 : 7;
txb_ctx->txb_skip_ctx = ctx_base + ctx_offset;
```

Two rows of the port's chroma table matter and the oracle census says both
fire. `EC_DBGCTX` on the oracle, whole stream, 9216 chroma units:

```
off=7   520 units      off=10   8696 units
```

and at the fork site itself (`mi=0,0`, the first inter block) every chroma unit
is `tx=0` (TX_4X4 — lossless forces it) with **`off=7`**:

```
EC_DBGCTX mi=0,0 plane=1 tx=0 above0=0 left0=0 base=0 off=7 ctx=7 cellsA=0, cellsL=0,
EC_DBGCTX mi=0,0 plane=2 tx=0 above0=0 left0=0 base=0 off=7 ctx=7 cellsA=0, cellsL=0,
```

`read_plane` (`decode.rs:19627-19636`) picks the band like this:

```rust
let skip_ctx = if plane_idx == 0 {
    luma_skip_ctx.unwrap_or(0)
} else {
    usize::from(around.0) + usize::from(around.1) + luma_skip_ctx.unwrap_or(0)
};
```

and `luma_skip_ctx` / `luma_skip_ctx_rect` (`decode.rs:9361-9411`) return the
**luma** formula `SKIP_CONTEXTS[top][left]` — a value that is being used here
as the chroma **offset band**. Its own doc comment says the `+3` that reaches
the offset-10 rows applies "only [to] a 128x128 block's TX_32X32 chroma units,
lane-sb128b r3". **H5 is a 128x128 root with TX_4X4 units**, i.e. exactly the
combination that comment does not cover, and it is the only committed-free cell
where a 128-root meets lossless. That is where to look first: whether the
offset-10 band is being selected (or the offset-7 band) for a 128-root's
lossless chroma units, and whether `ctx_base` should be
`get_entropy_context` rather than the luma `SKIP_CONTEXTS` table.

Note the port's table is **reindexed**: our chroma rows 0..2 stand for libaom's
7..9 and our 3..5 for libaom's 10..12, so a raw `ctx` comparison between the
two traces is meaningless without that `+7`. Verified on the first two
coefficient units of the stream (ours `ctx=0` ↔ oracle `ctx=7`).

## 6. Is this Levent's H1 class?

**No — different site, different trigger.** H1 is 4:4:4 **lossy** + a **rect**
partition, and its fork is a unit-ordering bug: the rect walk emits a second
luma unit where the oracle moves to the chroma plane (the `EC_COEFF_STEP`
`plane=0` label on our side is a label bug there). H5 is 4:4:4 **lossless** +
a **square 128 root**, and its fork is a single wrong `txb_skip` CDF row inside
one unit — the unit walk itself is aligned (the `all_zero` reads pair up with
identical `rng` on both sides for the whole prefix). Same broad neighbourhood
(4:4:4 chroma coefficient context), different defect. Notified
`agent://Levent-2` so it is not fixed twice.

### 6b. The H1/H5 split is formula-halves, and the arms are disjoint

`Levent-2`'s H1 root cause (confirmed to me directly): the 1:4-pair chroma
gather runs at the 4:2:0 PAIR extent (16x8 / 8x16), where
`is_chroma_reference` reduces both parity clauses to `!ss_y` / `!ss_x`, so the
strip is its own chroma reference and the gather reads one extra luma mi row of
LEFT context. Their `ctx_base` came out 1 (0 above + 1 left) where the oracle
read offset-7 row 7 (= `ctx_base` 0). Pinned fixture 6441 B,
sha256 `77f727e7...`, now byte-exact on their branch.

The two lanes split ONE formula along its two terms:

```
txb_skip_ctx = get_entropy_context(tx_size, a, l)      <-- H1: the ctx_base term
              + (plane_bsize > tx block ? 10 : 7)      <-- H5: the BAND/OFFSET term
```

**The arms are structurally disjoint, not merely different.** Levent's fix is
the match arm `Some(_) if ss_x(fctx) == 0 => around` inside `match
strip_chroma`, and `strip_chroma` is `Some` only from the single setter
(`decode.rs:50852`) that the `PARTITION_HORZ_4` / `VERT_4` 16x4 / 4x16
`inter_piece` loop runs. A `BLOCK_128X128` unsplit root never reaches that
loop, so `mi(0,0)` on this 128-root stream cannot move through Levent's gate.
They are re-running this exact recipe (63429 B, sha256 `bbffb979...`) against
base and their tip to turn that argument into a measured "unchanged".

**Caveat for whoever measures:** H5 was characterized on a tree that does NOT
contain Levent's fix. The `--max-partition-size=64` control being byte-exact
proves the 128 root is necessary, but on its own it says nothing about the
`ctx_base` half at that root. Take the re-measurement above as the authority.

## 7. What a follow-up lane should do, in order

1. Instrument the band selection in `read_plane` (a one-line
   `EC_DBGCTX`-shaped rung on our side: plane, tx_size, plane_bsize, the chosen
   offset) and diff it against the oracle's `EC_DBGCTX` for the first inter
   block. That single run should name the wrong term.
2. Fix one class: the 128-root lossless chroma band. Do **not** touch the rect
   unit walk (H1's territory).
3. Re-run the two arms of §2. Both must be byte-exact.
4. Then arm a gate: the recipe above at `--cpu-used=0`, `ss (0,0)` + 10-bit
   header assert, plus a counter on the corrected route so the gate is not
   blind, and a mutation proof (flip the band back → the gate must go red on
   the first inter frame, byte 74769).
5. Re-measure the committed 4:4:4 / 4:2:0 pins afterwards.

## 8. State

No commit. `~/.cache/wt/av1h5` carries no source edits — the work was
measurement only (EC_SYMR / EC_TRACE_COEFF / EC_DBGCTX traces, all env-gated
and pre-existing). No `EC_AV1_ALLOW_422_PROBE` bypass was applied or needed.
