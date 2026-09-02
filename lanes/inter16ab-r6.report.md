# lane-inter16ab r6 — report (RED, root cause NOT found)

GOAL: lift "a non-skip rectangular (HORZ/VERT/HORZ_B) strip needs rectangular
residual coding" for the 32-level 1:4 inter strips (32x8 / 8x32) — the shape
the 2160p 10-bit film stops on at -ss 900.

## 1. What was built (r6 partial from the interrupted round, commit 9963480 + this one)
- `crates/ec-av1/src/cdf_state.rs`: `TxbSet::LumaRect32x8Inter` — side-16 coefficient
  tables (`get_txsize_entropy_ctx(TX_32X8)` = (txsize_sqr_map 1 + txsize_sqr_up_map 3 + 1)>>1
  = the 16x16 class), `eob_pt_256` (`txsize_log2_minus4[TX_32X8] == 4`), `tx_type` =
  `inter_tx_type_8` (`av1_get_ext_tx_set_type`, blockd.h:1097: `tx_size_sqr_up == TX_32X32`
  + inter → `EXT_TX_SET_DCT_IDTX` for BOTH `reduced_tx_set` values, read at the
  `txsize_sqr_map[TX_32X8] == TX_8X8` CDF row).
- `crates/ec-av1/src/decode.rs`: `(32,8)|(8,32)` added to `rect_inter_residual_supported`,
  `rect_inter_luma_set`, and `(16,4)|(4,16) => TxbSet::Chroma8` in `rect_inter_chroma_set`
  (`ss_size_lookup[BLOCK_32X8] == BLOCK_16X4`, `get_txsize_entropy_ctx(TX_16X4)` = 8x8 class,
  area 64 → `eob_pt_64`); scan (`SCAN_32X8`/`SCAN_8X32`) and the rect inverse transform were
  already generic. Per-shape counter `RECT32X8_INTER_TU_HITS` → `stream::rect32x8_inter_tu_hits()`
  → `decode_probe`.
- `crates/ec-av1/src/stream.rs`: new gate
  `a_real_aomenc_inter_sequence_with_32x8_rect_strips_decodes_pixel_exact`.

## 2. Film re-probe — the refusal IS gone, the film advances one blocker
`~/.cache/inter16ab-tmp/hg900.obu` (10-bit 2160p, -ss 900, 2 s):
- r5 stop: `a non-skip rectangular (HORZ/VERT/HORZ_B) strip needs rectangular residual coding`
- r6 stop: `an inter partition below 8x8 (this decoder codes no inter leaf smaller than 8x8; lane-sub8 scoped to intra)`
- counters at the stop: `rect32x8_inter_tu: 32x8=10 8x32=3`, still 0 frames output (104 frame headers).

EVIDENCE: ~/.cache/inter16ab-tmp/hg900.obu | `EC_AV1_FINAL_DUMP=1 systemd-run --user --scope -q -p MemoryMax=6G decode_probe hg900.obu` | stop string moved to the lane-sub8 refusal; 13 TX_32X8/TX_8X32 inter luma TUs coded before it.

## 3. WHY THIS IS RED — the new path desyncs on a real stream
The 16x16-level 1:4 gate's 8-bit attempt 1 stream (previously refused at the
rect-residual string, now walking past it) decodes 6 frames but DIVERGES:

- repro: `~/.cache/inter16ab-tmp/g14.sh` → `g14.obu` (192x128, 6 frames, 8-bit, cq 18,
  `--enable-tx-size-search=1 --enable-rect-partitions=1 --enable-ab-partitions=1
  --enable-1to4-partitions=1 --min-partition-size=4 --max-partition-size=16`)
- ours vs ffmpeg: frames 0-2 exact; frame 3: 8317 luma pixels differ (max |d| 99, first at
  (97,61)); frames 4-5: ~24 400 pixels (whole-frame band = entropy desync, not rounding).
- that stream codes `rect32x8_inter_tu: 32x8=2 8x32=0` — i.e. the divergence coincides with
  the ONLY newly-enabled path in it. Not bisected further (turn cap): the open question is
  whether the 32x8 TU read itself is wrong (tx_type row / eob_pt / txb_skip ctx) or whether
  it is read out of an already-diverged stream.

Consequences in the suite: `a_real_aomenc_inter_sequence_with_16x16_level_1to4_partitions_decodes_pixel_exact`
now FAILS (its `oos_mismatch` assert catches exactly this stream, and its
`|| rect_residual_refusals > 0` escape is gone because the refusal is lifted). That failure is
the gate doing its job, not noise.

EVIDENCE: ~/.cache/inter16ab-tmp/g14.obu | `bash g14.sh; EC_PROBE_OUT=g14.yuv decode_probe g14.obu; ffmpeg -i g14.obu -f rawvideo g14f.yuv` + per-frame python compare | frame 3 8317 luma px differ, max |delta| 99, with 32x8=2 coded rect TUs in the stream.

## 4. Second, separate defect found (pre-existing, NOT this round's code)
8-bit Y-structured sinusoid, cq 22, `--enable-tx-size-search=0`
(`~/.cache/inter16ab-tmp/a8.sh`): 4 luma pixels of frame 1 differ by +-1 (cols 48/54/55/59,
rows 121/125/126 — inside the bottom 32x8 strip row), propagating as 1 px/frame afterwards.
That stream codes ZERO rectangular transform units (all its 32x8 strips are SKIP), so no code
from this round runs in it. Re-encoding the identical source with `--enable-cdef=0` decodes
**pixel-exact**, which localises it to CDEF over a SKIP 32x8 strip.

EVIDENCE: ~/.cache/inter16ab-tmp/a8.sh, a8n.sh | encode identical source with and without `--enable-cdef=0`, decode both, byte-compare vs ffmpeg | CDEF on: 4 px +-1 in frame 1 (then 1 px/frame); CDEF off: 0 differing bytes over 6 frames.

## 5. Gate recipe search (measured, for the successor)
128 synthetic streams swept (`~/.cache/inter16ab-tmp/sweep.sh`, `sweep2.sh`, `sweep3.sh`,
`sweep4.sh`; two source families x cq 16..55 x tx-size-search 0/1 x 8+10 bit x
`--max-partition-size` 32/64): exactly ONE fires a coded rect 32-level TU
(8-bit, X-structured `128+60*sin((X+N*3)/6-style)` source, cq 22, tx-size-search 0 →
`8x32=2`, pixel-exact). ZERO synthetic streams ever produced a coded **32x8** unit; the film
produces 10 in 2 s. Banded + `noise=all_seed=7` sources are useless here — they refuse on four
other lanes' gaps (below-8x8 inter leaf, non-DC chroma on an 8x8 inter leaf, intra 16x4 strip,
SB AB partition). Also measured: `--enable-ab-partitions=0` streams still refuse at
"an inter SB-level AB partition" in this decoder (8 x 16x64 strips read first) — a separate
pre-existing gap, worth a lane.

## 6. Refusal inventory — NOT dropped, deliberately
The string still guards two live cases: any rect shape outside
`rect_inter_residual_supported`, and a strip decoded with SQUARE dimensions
(`write_w == side && write_h == side`, the lane-warp r5 HORZ_B corner-cut). Dropping the
inventory line would be a false capability claim. Deviation from the charter, stated here.

## Residue
- fix-now: the g14.obu frame-3 divergence (section 3). Until it is root-caused this lane's
  lift is NOT provable and the 1:4 gate stays red.
- fix-now (another lane / same round): the CDEF-over-skip-32x8-strip +-1 defect (section 4).
- accepted: the 32x8 (HORZ_4) orientation has no synthetic live carrier; the new gate asserts
  only the 8x32 one and says so in the source.
