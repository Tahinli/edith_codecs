# lane-vartxsplit r2 — the 10-bit case was stale; the gate's firing recipe was vacuous

Branch `lane-vartxsplit`, merged with main (beecb64) at `335ccdc` (the r1 compound-8x8
`saved_luma_ctx` fix landed independently on main via lane-sub8x4; the merge kept main's
copy — same semantics, same call site).

## (1) The 10-bit 16x32 case (gaterecipe recipe 2) — VERDICT: already fixed by the merge

Streams (each hashed twice, identical): `~/.cache/vartxsplit-tmp/s10.obu`
md5 `99980afd5945c61621d38c4e5c104db9`, transposed twin `s10v.obu`
md5 `93890af7b420886beb5394bf30f6f805` (384x256, 6 frames @25, yuv420p10le, cq 10,
gate A's flag set, `--min-partition-size=8 --max-partition-size=64`, generator
`~/.cache/vartxsplit-tmp/gen10.sh` / `gen10v.sh`).

- At `cba81e6` (pre-merge): `s10` differed from ffmpeg at decode-order frames 4-5,
  `s10v` at frames 2-5.
- At `335ccdc` (post-merge): **all six frames of both streams are byte-identical to
  `ffmpeg -pix_fmt yuv420p10le`, Y/U/V** — no new fix was needed. The premise expired
  with lane-sub8x4 (class stale-premise-lanes).
- The recipe also carries **no 16x32 var-tx leaf at all**: aomdec's `EC_VARTX` histogram
  of both streams reads only tx_size 1/2/7/8 split symbols — zero `TX_16X64`/`TX_64X16`
  (and zero `TX_8X32`/`TX_32X8`). The "46 16x32 leaves" gaterecipe r1 recorded were
  counted inside an already-desynced decode.

EVIDENCE: ~/.cache/vartxsplit-tmp/s10.obu + s10v.obu | aomenc (twice, equal md5) -> dump_yuv -> ffmpeg yuv420p10le rawvideo compare, 6 frames x 3 planes | 12/12 frames byte-identical, oracle EC_VARTX 16x32/32x16 split symbols = 0

Same-shape sweep asked for in the charter (`record_mi` with a placeholder grid after a
split): `grep -n "saved_luma_ctx"` in decode.rs shows both 8x8 leaf arms (single-ref
`decode_leaf8` and the compound arm) now save/restore, and they are the only
`record_mi(..., 8, ...)` call sites that follow a `split8` transform; the rect strip and
sub8 rect2/split4 arms publish their own per-TU state and never rewrite a placeholder
grid over it.

## (2) Gate A — `real_aomenc_1to4_streams_..._rect_vartx_leaves_fire_before_a_named_refusal`

RED on the merged tree with "leaves decoded anywhere 32x16=0 16x32=0". Root cause is the
RECIPE, not a counter placement: the firing attempts' cq-10 stream contains **no
rectangular sub-transform** (oracle `EC_VARTX`, above), so the counter is right and the
old 21/15 reading came from a desynced decode (class counter-from-refused-stream, second
instance in this gate).

Re-measured (`~/.cache/vartxsplit-tmp/sweep2.sh`, `sweep3.sh`, ~40 aomenc runs; oracle
histogram of tx_size 15/16/17/18 with val=1): the same product texture at **cq 44 and 46**
is what splits a rectangular transform — `TX_32X8`/`TX_8X32` splits giving **16x8 and 8x16
rect var-tx leaves**, both orientations out of one stream — and both streams decode
pixel-exact. So attempts 16/17 now run cq 44 / cq 46 on the horizontal source, and the
firing assert was **strengthened**: it asserts the PIXEL-COMPARED tally
(`leaf_32x16 > 0 && leaf_16x32 > 0`), not "decoded anywhere".

EVIDENCE: ~/.cache/vartxsplit-tmp/p_44.obu, p_46.obu | aomenc -> EC_VARTXLEAF decode -> ffmpeg yuv420p compare | ours 3x 16x8 + 1x 8x16 leaves each, 6/6 frames exact (oracle: tx15/1=1 tx16/1=3 at cq 44)

## (3) Gate B — the split-tx-8x4 arm stays reported-unproven (DEVIATION from the charter)

The charter asked for a hard assert. It cannot be honest today, and this is measured, not
assumed:
- gate B's own source family never splits a 16x4/4x16 transform at ANY quantiser: 12 fresh
  streams (both orientations, both motion steps, cq 46/56/63, `--min-partition-size=4
  --max-partition-size=16 --enable-tx-size-search=1`) carry **zero** TX_16X4/TX_4X16 split
  symbols in aomdec's `EC_VARTX` histogram, and all 12 decode pixel-exact;
- the recipe gaterecipe r1 credited with 6 split-tx 8x4 leaves (`s8.obu`, md5
  `842355ef494455ead0352f62f84f2c56`) now decodes **pixel-exact on all six frames with
  ZERO rect var-tx leaves** — those 6 were read out of the desync lane-sub8x4 fixed;
- the product texture at `--min-partition-size=4` DOES split it heavily (140 TX_16X4 +
  238 TX_4X16 at cq 44), but every one of those streams either raises a refusal our own
  desync produces ("an OBMC neighbour whose switchable interp filter…", "a Golomb tail
  longer than this decoder reads" — both with `--enable-obmc=0 --enable-dual-filter=0` on
  the command line, i.e. class refusal-from-own-desync) or mismatches ffmpeg from
  decode-order frame 1.

EVIDENCE: ~/.cache/vartxsplit-tmp/sweep5.sh + sweep6.sh logs | 24 aomenc streams -> aomdec EC_VARTX histogram + our decode + ffmpeg compare | gate-B family 12/12 exact with 0 TX_16X4 splits; product-texture min-part-4 family 0/12 exact

The gate's comment now carries this measurement instead of gaterecipe's stale one.

## (4) Open defect handed off (not mine to close this round)

384x256 product texture, `--min-partition-size=8 --max-partition-size=64`, transposed
source at cq 56 (`pv_56.obu`) — 8-bit, mismatches ffmpeg from decode-order frame 2 with
ZERO rect var-tx leaves. Triaged:
- **entropy is EXACT**: the deduped msac range sequence of frames 0-2 vs instrumented
  aomdec has 243696 insertions (our extra `br` prints after `base_eob`) and **0 deletions**
  — we read exactly the symbols the reference wrote. It is a RECONSTRUCTION defect.
- shape: luma columns **8..15** (one 8-pixel-wide strip), rows 124..224, 241 samples,
  delta -17..+2, plus one 8x4 chroma unit at (108,88) in both U and V, delta -16.
The 10-bit twins behave the same way (`t10_46`, `t10v_48`, `t10v_52`: mismatch with zero
rect leaves; `t10v_40` fires 2+2 leaves and mismatches only on frame 5). This is why no
10-bit firing arm was added to gate A — deferred(the 8-wide-strip reconstruction defect
above), it would make the gate's own `oos_mismatch` assert red.

## Instrumentation added
`EC_VARTXLEAF=1` (decode.rs `vartx_rect_leaf_hit`): prints `tw=`/`th=` per rectangular
var-tx leaf — this is what separated "our counter moved" from "the recipe never coded the
shape" in one run.

## Gate + suite results (merged tree, `335ccdc` + this lane's two commits)

```
real_aomenc_1to4_streams_decode_pixel_exact_and_rect_vartx_leaves_fire_before_a_named_refusal
 (8-bit): 0 named refusals, 2 pixel-exact attempts carrying the arm,
 pixel-compared rect var-tx leaves 32x16=6 16x32=2, leaves decoded anywhere 32x16=6 16x32=2,
 16 attempts carried none (0 of them mismatched)
 (10-bit): 0 named refusals, ... 16 attempts carried none (0 of them mismatched)
a_real_aomenc_inter_sequence_with_16x16_level_1to4_partitions_decodes_pixel_exact
 (8-bit): 0 named refusals, 2 pixel-exact attempts, per-arm
 HORZ_4/VERT_4/chroma-pair/sub8x8-chroma/split-tx-8x4=[1, 1, 2, 2, 0], 30 carried none (0 mismatched)
 split-tx-8x4 arm unproven (arms [1, 1, 2, 2, 0])
named gates + refusal_inventory + gate_coverage + sub8 + obmc:
 test result: ok. 26 passed; 0 failed; 0 ignored; 435 filtered out; 135.30s
full suite: test result: ok. 424 passed; 0 failed; 37 ignored; 0 measured; 855.00s
```

EVIDENCE: /home/tahinli/.cache/vartxsplit-gates.log + /home/tahinli/.cache/vartxsplit-suite.log | cargo test -p ec-av1 --lib (named gates, then whole lib, both as systemd user units) | 26/26 named green, suite 424 passed 0 failed 37 ignored

## Residue
- deferred: the 8-pixel-wide-column reconstruction defect (section 4) — entropy proved
  exact, so it is a prediction/filter-stage bug — unblocked by a lane that owns the
  8xN inter reconstruction path; it is what keeps a 10-bit firing arm out of gate A.
- deferred: the split-tx-8x4 hard assert — unblocked by that same defect (it is what
  makes every `--min-partition-size=4` product-texture stream red).
- accepted: gate A's leaves are 16x8/8x16 (`TX_32X8`/`TX_8X32` splits), not the 32x16/16x32
  its variable names say; no `TX_64X16`/`TX_16X64` split exists in any recipe measured in
  ~70 aomenc runs across r1+r2. The counter is one generic "rect var-tx leaf with both
  sides >= 8" pair; renaming it is churn this lane did not spend.
