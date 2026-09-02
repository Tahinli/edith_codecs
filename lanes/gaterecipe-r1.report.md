# lane-gaterecipe r1 — two 1:4 gates that were red on their FIRING assert

Both gates were red with **0 pixel mismatches and 0 refusals**: the shape each
claimed simply was not in its streams once the refusals their recipes used to
stop at were lifted (sbab, rectres).

## Gate A — `real_aomenc_1to4_streams_decode_pixel_exact_and_rect_vartx_leaves_fire_before_a_named_refusal`

VERDICT: **vacuous recipe** (class `counter-from-refused-stream` + `gate-blind-to-feature`).

Step 1, the old recipe measured against the ORACLE. `aomdec` with
`EC_TRACE=1 EC_TRACE_COEFF=1` on the stream the old firing arm produced
(384x256, 16-px banded translation, cq 32..58, `--min-partition-size=16`):

| oracle histogram | reading |
|---|---|
| partitions `(bsize,p)`: `(9,2)x424 (12,3)x119 (9,0)x32 (12,0)x25 (9,6)x9 (9,7)x7` | **zero 1:4 partitions at any level** |
| plane-0 coefficient `tx_size`: `tx2:39 tx4:25 tx9:218` | whole TX_16X16/TX_64X64/TX_16X32 units, **zero sub-transforms** |

So the encoder never wrote the shape; the leaves the old assert counted came out
of the two attempts that stopped at a named refusal. Our decoder agrees with the
oracle: on the merged tree those 18 attempts decode pixel-exact with the counter
at 0.

EVIDENCE: /home/tahinli/.cache/gaterecipe-g1.log + ~/.cache/gaterecipe-tmp/g1c50.trace | old recipe, 18 attempts x 2 depths decoded + aomdec EC_TRACE/EC_TRACE_COEFF histogram | 0 rect var-tx leaves ours, 0 1:4 partitions and 0 sub-transforms oracle-side

Step 2, recipe re-sweep (~40 aomenc runs, script `~/.cache/gaterecipe-tmp/sweep.sh`,
oracle histogram `~/.cache/gaterecipe-tmp/oracle.sh`): banded-motion sources at any
cq/size and `--min-partition-size` 16 never split a rectangular transform. What does:
a high-frequency PRODUCT texture `128+90*sin((X+N*3)/6)*sin(Y/2)+50*sin((X*Y)/37)`
at 384x256, **cq 10**, `--min-partition-size=8 --enable-tx-size-search=1`
(part of a block predicts well and part does not = the RD case for `txfm_partition`).

Gate A now uses that recipe on attempts 16/17 (plus its transposed twin), 8-bit only:

```
real_aomenc_1to4_streams_..._rect_vartx_leaves_fire_before_a_named_refusal (8-bit):
  2 named refusals, 0 pixel-exact attempts carrying the arm,
  pixel-compared rect var-tx leaves 32x16=0 16x32=0,
  leaves decoded anywhere in the sweep 32x16=21 16x32=15,
  16 attempts carried none (0 of them mismatched)
(10-bit): 0 named refusals, ... 16 attempts carried none (0 of them mismatched)
test result: ok. 1 passed
```

Both firing attempts stop at another lane's live refusal ("an intra 16x4/4x16 strip
inside an inter 16x16-level 1:4 partition"), which is exactly what the gate's NAME
claims (leaves fire *before a named refusal*) and what its `fired_*` vs `leaf_*`
split already separated. Every attempt that ran to the end is compared Y/U/V per
decode-order frame, `oos_mismatch == 0` asserted.

EVIDENCE: /home/tahinli/.cache/gaterecipe-g1d.log | 34 aomenc streams (18 8-bit + 16 10-bit) -> decode -> ffmpeg Y/U/V compare | 36 rect var-tx leaves decoded before a named refusal, 0 mismatches, gate green

## Gate B — `a_real_aomenc_inter_sequence_with_16x16_level_1to4_partitions_decodes_pixel_exact`

VERDICT: **vacuous recipe for the split-tx-8x4 arm** + an **open decode defect** on the
only recipe that fires it.

The arm's escape hatch ("some attempt refused on rectangular residual coding") closed
when lane-rectres lifted that refusal, and the arm itself has always read 0. Measured:
no 192x128 recipe of this sweep (cq 8..44, both orientations, `--min-partition-size=4`,
`--enable-tx-size-search=1`) makes aomenc split a 16x4/4x16 strip's transform; the
oracle histograms carry whole TX_16X4/TX_4X16 units and no sub-transform.

The 384x256 cq-10 recipe above DOES fire it — `arms [4, 4, 4, 4, 6]`, six 8x4/4x8
var-tx leaves — and that stream **MISMATCHES ffmpeg at decode-order frame 2 (luma)**.
So the arm cannot be pixel-proved today; the gate now reports it unproven (eprintln +
comment naming the sweep) instead of asserting a shape no decodable recipe produces,
and the VERT_4 arm keeps its hard assert.

```
(8-bit): 0 named refusals, 2 pixel-exact attempts carrying a 16-level inter 1:4 partition,
  per-arm HORZ_4/VERT_4/chroma-pair/sub8x8-chroma/split-tx-8x4=[1, 1, 2, 2, 0],
  30 attempts carried none (0 of them mismatched)
split-tx-8x4 arm unproven (arms [1, 1, 2, 2, 0])
test result: ok. 1 passed
```

EVIDENCE: /home/tahinli/.cache/gaterecipe-g2c.log (mismatch) + /home/tahinli/.cache/gaterecipe-g2d.log (green) | 384x256 cq-10 min-partition-size=8 arm decoded and compared | 6 split-tx 8x4 leaves fired, frame 2 luma differs -> arm withdrawn, defect handed off

## HANDED-OFF DECODE DEFECT (do not lose this)

Two independent recipes decode a rectangular var-tx SPLIT wrong, silently (no refusal):

1. 8-bit, 384x256, 6 frames @25, source `128+90*sin((X+N*3)/6)*sin(Y/2)+50*sin((X*Y)/37)`,
   `--codec=av1 --passes=1 --end-usage=q --cq-level=10 --cpu-used=0 --threads=1 --row-mt=0
   --sb-size=64 --enable-restoration=0 --enable-palette=0 --deltaq-mode=0
   --enable-filter-intra=0 --enable-cfl-intra=0 --enable-intrabc=0 --lag-in-frames=0
   --kf-max-dist=9999 --tile-columns=0 --enable-tx-size-search=1 --enable-rect-partitions=1
   --enable-ab-partitions=1 --enable-1to4-partitions=1 --min-partition-size=8
   --max-partition-size=64` -> arms [4,4,4,4,6], **frame 2 luma differs**.
2. 10-bit, same source at cq 10, gate A's flag set (`--enable-ab-partitions=0
   --enable-dual-filter=0 --enable-obmc=0 --enable-tx64=1`) -> 46 16x32 leaves on
   attempt 16, **frame 2 luma differs** on attempt 17.

fix-now for the decode lane that owns `read_block_tx_size` / `read_inter_plane_rect`;
this lane may not touch decode.rs.

## Also fixed, same diff

`aomenc stdin is fed from a thread` in gate A: the single-threaded
`write_all` + `wait_with_output` shape deadlocks as soon as aomenc's stdout pipe fills
before it has consumed the whole y4m. The cq-10 arm hit it — one encode hung 40 minutes
(class: gate loader/pipe deadlock, 2nd instance in this repo).

## Residue

- deferred: the split-tx-8x4 arm and the pixel-exact proof of 32x16/16x32 var-tx leaves —
  blocked on (a) the "intra 16x4/4x16 strip inside an inter 16x16-level 1:4 partition"
  refusal and (b) the decode defect above. Unblocked by either landing.
- accepted: gate A's leaves are proved DECODED, not pixel-exact, exactly as its name says.
