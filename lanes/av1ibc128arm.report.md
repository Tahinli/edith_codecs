# Lane av1ibc128arm: 4:4:4 intra-in-inter 128-root mu-chunk chroma unit walk

Base: main `a21f3680`. Branch `lane-av1ibc128arm`, worktree
`/home/tahinli/.cache/wt/av1ibc128arm`. Oracle: `~/.cache/aom-oracle/build/{aomenc,aomdec}`
(aom `v3.13.3-7-g9bb526a`).

## Verdict: the ticket's part (1) is REFUTED by the oracle; parts (2) and the
## per-unit walk are the real defect, and the fix is green

The ticket said "`cu_tx` must be `32 << ss_x` ... per `blockd.h:1372`,
`av1_get_max_uv_txsize(BLOCK_128X128, 0, 0)` is TX_64X64, so at 4:4:4 `cu_tx` is
64, not 32". **That is wrong for this aom.** `blockd.h:1371` is

```c
static inline TX_SIZE av1_get_max_uv_txsize(BLOCK_SIZE bsize, int subsampling_x,
                                            int subsampling_y) {
  const BLOCK_SIZE plane_bsize = get_plane_block_size(bsize, subsampling_x, subsampling_y);
  const TX_SIZE uv_tx = max_txsize_rect_lookup[plane_bsize];
  return av1_get_adjusted_tx_size(uv_tx);
}
```

and `av1_get_adjusted_tx_size` (`blockd.h:1361`, read in the oracle tree at
`~/.cache/aom-oracle/src/av1/common/blockd.h:1361`) takes **no subsampling
argument** — it maps `TX_64X64 / TX_64X32 / TX_32X64` to `TX_32X32`
**unconditionally**. So even at ss (0,0), where `get_plane_block_size` returns
`BLOCK_128X128` and `max_txsize_rect_lookup[BLOCK_128X128]` is `TX_64X64`, the
answer is **TX_32X32**. The chroma unit is 32 at every subsampling.

**Oracle measurement** (`EC_TRACE_COEFF=1 aomdec` on the gate's own 4:4:4
stream, `h_min_128x256_n3.obu`): every chroma read of the 128x128
intra-in-inter root is `tx_size=3` (TX_32X32) — four per plane per mu chunk, at
`(row,col) (0,0) (0,8) (8,0) (8,8)`, interleaved plane-major per chunk after
the chunk's single `tx_size=4` (TX_64X64) luma unit. Never a TX_64X64 chroma
unit. The ticket's part (1) would have read a 64x64 transform where libaom
reads four 32x32 ones.

## The real defect (`decode_inter_block`, the `side > 64` intra-in-inter arm)

The arm had three 4:2:0 literals standing in for four different quantities:

| quantity | old (4:2:0 only) | correct |
| --- | --- | --- |
| chroma unit side `cu_tx` | `32` | `32` at every ss (correct already) |
| unit's LUMA span | `luma_span = cu_tx * 2` (64) | `cu_tx << ss_x` → 64 at 4:2:0, **32** at 4:4:4 |
| unit's CHROMA origin | `cpx + cc * cu_tx` | `cpx + cc * (64 >> ss_x)` → **+64** per chunk at 4:4:4 |
| units per mu chunk per plane | 1 | `chunk_chroma / cu_tx` → 1 at 4:2:0, **4** at 4:4:4 |

At 4:4:4 the old arm therefore (a) read one 32x32 unit where libaom codes four,
(b) stamped the unit's entropy context over the 4:2:0 64x64 luma span instead of
the unit's own 32x32, and (c) placed chunk 1's unit at `cpx + 32` — back inside
chunk 0's samples.

The fix is the per-unit walk the **inter** sibling already got
(`decode.rs:41347`, `chunk_chroma_w = 64 >> ss_x`, `units_w = chunk_chroma_w /
cu_tx`, `unit_luma_w = cu_tx << ss_x`), applied to the intra arm. The repaired
4x4-chroma siblings at `decode.rs:17598` / `17685` and the block-walk reference
give the same ss-aware form (`ox << ss_x(fctx)`, never `ox * 2`). No third shape
was invented. The lossless sub-arm's `(cc * cu_tx, cr * cu_tx)` / `(cu_tx, cu_tx)`
region args and the `mu_units` write-back offset were generalized the same way
(`chunk_chroma_w/h`), matching the inter arm's lossless sub-arm at 41409.

## Counter table

New counter `intra_128_in_inter_mu_chroma_hits()` (one bump per mu-chunk arm
firing), measured on the gate's 128x256 3-frame 4:4:4 stream and on a 256x256
8-frame 4:4:4 stream, both freshly encoded live:

| build | 128x256 (1 intra-128 root) | 256x256 (2 intra-128 roots) |
| --- | --- | --- |
| before fix (arm reverted, counter kept) | `mu_chroma=4`, `intra128=1` | `mu_chroma=24`, `intra128=6` |
| after fix | `mu_chroma=4`, `intra128=1` | `mu_chroma=8`, `intra128=2` |

The counter **fires in both builds** — the red is on pixels, not on the counter.
On the 256x256 witness the pre-fix count is *higher* (24 vs 8) because the
one-unit-per-chunk arm desyncs and the subsequent frames take different paths;
the minimal 128x256 witness is the clean one-unit-per-chunk case (4 = 1 root ×
2 mu chunks × 2 planes on the last frame, 8 on the 256x256 = 2 roots × 2 chunks
× 2 planes).

## Recipe (128x256, 3 frames — the gate encodes it live)

`rate=25`, `--limit=3`, `--cpu-used=0`, `--cq-level=62` and
`--min-partition-size=64 --max-partition-size=128` are all load-bearing.
Measured over 24 recipe/cell combinations: a clean `testsrc2` source never
produces an intra-coded 128 root on an inter frame; noise-on-`gradients` does,
and only at this seed/cq/geometry. At `--limit=2` the same recipe stops choosing
the shape entirely (0 fires).

```text
ffmpeg -f lavfi -i "gradients=size=128x256:c0=..:c1=..:c2=..:c3=..:seed=63:\
duration=0.12:rate=25,noise=all_seed=63:alls=6:allf=t" \
       -pix_fmt yuv444p -t 0.12 -f yuv4mpegpipe - | \
aomenc --codec=av1 --profile=1 --passes=1 --end-usage=q --cq-level=62 \
       --cpu-used=0 --threads=1 --row-mt=0 --sb-size=128 --limit=3 \
       --max-partition-size=128 --min-partition-size=64 \
       --enable-rect-partitions=0 --enable-ab-partitions=0 \
       --enable-1to4-partitions=0 --enable-palette=0 --enable-intrabc=0 \
       --deltaq-mode=0 --enable-tx-size-search=0 --obu -o out.obu -
```

## Header-parse evidence

The gate parses the encoded stream's own sequence header and asserts
`(use_128x128_superblock, subsampling_x, subsampling_y, mono_chrome) ==
(true, 0, 0, false)` **before** anything else, so a stream that silently came
back 4:2:0 cannot leave the arm unexercised while the rest passes.
`decode_probe` on the same stream: `SEQ: use_128x128_superblock=true bit_depth=8
mono_chrome=false max_frame=128x256`.

## Gate

`stream.rs::a_real_aomenc_444_intra_in_inter_128_root_codes_chroma_per_mu_chunk_unit_pixel_exact`

- encodes live with the oracle aomenc (no pinned fixture — the recipe is the
  witness, and a recipe that stops firing fails rather than passing vacuously),
- asserts the 4:4:4 + 128-superblock sequence header,
- `intra_128_in_inter_hits() >= 1` (a real intra-coded 128 root on an inter
  frame) and `intra_128_in_inter_mu_chroma_hits() >= 4` (the per-unit walk ran),
- `decode_all_frames_vs_oracle` — every decode-order frame compared
  byte-for-byte against the oracle, panicking on a refusal or any differing byte.

**Green:** `3 decode-order frames pixel-exact (0 hidden), intra_128_in_inter=1
mu_chunk_chroma_reads=4`.

On the 256x256 8-frame 4:4:4 witness all **9** decode-order frames are
byte-exact (`EC_AV1_FINAL_DUMP` ours vs oracle `aomdec`, 196608 B each, 0
differing bytes per frame).

## Red-before

Reverted **only** the arm body (the per-unit walk and the ss-aware arithmetic)
back to the pristine `a21f3680` text, keeping the counter and the new gate.
Restored afterwards with `cp` from a saved copy and verified with `diff -q`
(`RESTORED byte-identical`) — never `git checkout --`.

Gate-level, fresh run of the new gate on the reverted arm:

```
a_real_aomenc_444_intra_in_inter_128_root_..._pixel_exact: decode-order frame 2
of 3 (3 shown, 0 hidden) differs from the oracle at byte 15450 (ours 142 vs
141), 44649 bytes differ
test result: FAILED. 0 passed; 1 failed
```

It fails on **pixels**, with the counter still reading 4 — so the red is not
merely the counter. Direct dump comparison on the same reverted build:

| stream | decode-order frames | differing bytes |
| --- | --- | --- |
| 128x256 (red-before) | 3 | 0, 0, **44649** (frame 2) |
| 256x256 (red-before) | 9 | 0, 0, 0, 0, **167832**, **152747**, **193206**, **182687**, **190186** |

## 4:2:0 byte-identity (invariant)

The arm is unchanged in behaviour at ss (1,1) by construction: `cu_tx` is
unchanged (32), `cu_tx << ss_x == cu_tx * 2` at ss (1,1), and
`chunk_chroma == chunk_luma >> ss_x == 32 == cu_tx` so `units_w == units_h == 1`
— the walk degenerates to exactly the one unit per mu chunk the arm always
coded. Proven by running two existing 4:2:0 128-root gates unchanged on this
tree:

| gate | result |
| --- | --- |
| `a_real_aomenc_inter_128x128_none_root_decodes_pixel_exact` | **ok** — 3 arms pixel-exact; 8-bit `intra_128_in_inter=0`, **10-bit `intra_128_in_inter=1`** (the arm really runs at 4:2:0 on that arm and stays exact), `chroma_units` 32/40/40; 3rd arm `intra_128_in_inter=0` |
| `a_real_aomenc_128x128_none_inter_blocks_coded_chroma_per_mu_chunk_decodes_pixel_exact` | **ok** — 12 decode-order frames pixel-exact, `inter_sb128_none_hits=11 chroma_units=88` |

The 10-bit arm of the first gate is the direct proof: it fires the
intra-in-inter 128 arm at ss (1,1) (`intra_128_in_inter=1`) and every frame is
byte-exact, so the 4:2:0 path through the rewritten walk is unchanged.

A targeted extra control: the same 128x256 recipe at `--profile=0` / `yuv420p`
produced 3 frames decoding cleanly (`SEQ: use_128x128_superblock=true
max_frame=128x256`, no refusal) with `ibc128: mu_chroma=0 intra128=0` — the
encoder picks no intra 128 root there, which is why the two existing gates above
are the 4:2:0 evidence rather than this stream.

## Files changed

- `crates/ec-av1/src/decode.rs` — new counter
  `INTRA_128_IN_INTER_MU_CHROMA_HITS` + accessor
  `intra_128_in_inter_mu_chroma_hits()`; the arm's per-unit walk.
- `crates/ec-av1/examples/decode_probe.rs` — prints the counter (scratch
  instrument, left in so the arm's engagement is inspectable from the probe).
- `crates/ec-av1/src/stream.rs` — the new gate.

`cargo build --release -p ec-av1 --tests --features gate-counters`: 0 errors,
0 warnings.

## Note for the next reader

The ticket's part (1) should not be re-applied anywhere: a TX_64X64 chroma unit
at 4:4:4 is not what libaom 3.13.3 codes, on this or any other arm. The
already-landed inter arm got this right; the intra arm is now the second
instance carrying the per-unit walk, and the ss-aware `cu_tx << ss` /
`chunk_luma >> ss` forms are the house pattern.
