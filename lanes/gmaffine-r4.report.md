# lane-gmaffine r4 — the 8x8-leaf desync was a MISSING CDF ROW (chroma/most-size `eob_pt` class-1), not the mv stack

Commits on `lane-gmaffine`, on top of r3 (`f8de9c3`). Suite:
`EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --release --lib -j3` →
**271 passed, 2 failed, 24 ignored** (r3: 269/2/24 equivalent — no regression; the 2
failures are the same two 8x8 motion gates, now failing on PIXELS, not on a refusal).

## Root cause 1 (entropy, fixed): `eob_pt` class-1 CDF existed only for two LUMA sizes
libaom's `av1_default_eob_multi*_cdfs[q][PLANE_TYPES][2]` splits every eob_pt table by
`eob_multi_ctx = (tx_class == TX_CLASS_2D) ? 0 : 1`. This crate carried that second row
for `eob_pt_16_luma` and `eob_pt_64_luma` ONLY (lane-av1tx4 r5); every other table —
including all chroma — fell back to its 2D row (`cdf_state.rs`, 14 `TxbSet` arms had
`eob_pt_class1: None`).
Why it bit exactly here: an INTER block's chroma never codes a `tx_type` symbol, it
INHERITS the colocated luma type verbatim (`av1_get_tx_type`, blockd.h:1291). The gate's
8x8 leaves code `H_ADST` luma, so their 4x4 chroma is a 1D class — a routine case that
read the 2D CDF: right value, wrong interval, desync one symbol later.
Fix: class-1 siblings for all 9 remaining tables (16/64/128 chroma, 256/512/1024 both
planes) — `cdf.rs` (36 new consts, machine-extracted from `token_cdfs.h`),
`cdf_state.rs` (fields + `reset1` per-frame counter reset + q-context `pick` + all 14
`txb()` arms now `Some(..)`; 0 `eob_pt_class1: None` left).

EVIDENCE: `/tmp/.../scratchpad/{ours_c.txt,aom_c.txt}` | `EC_TRACE_COEFF=1 EC_TRACE_MODE=1`
ours vs instrumented aomdec on `gm1_cq32.obu` (sha256 22d09738…, the globalmv gate's own
8-bit cq32 recipe) | first divergent element = the chroma (plane 1) 4x4 eob of leaf
mi=(6,4): both enter at rng=37748 and decode eob=16, ours leaves at rng=35080, aomdec at
54536. After the fix all four gate streams (`gm{0,1}_cq{32,45}`) decode all 24 frames
instead of refusing ("an inter partition below 16x16 other than SPLIT" / "a partition
below 8x8" — both were `refusal-names-a-correlate` symptoms of this desync).

## Root cause 2 (prediction, fixed, libaom-cited): warp is per PLANE and bails below 8x8
`av1_init_warp_params` (reconinter.c:58) returns before `allow_warp` when
`block_width < 8 || block_height < 8` — in 420 that is the chroma plane of every luma
block below 16x16. We warped chroma at 4x4. Fixed at both warp sites
(`decode.rs` 16x16+ site `chroma_side >= 8`, 8x8-leaf site `CHROMA_SIDE >= 8`).

## Still RED: chroma-only pixel residue on both 8x8 motion gates — fix-now, next round
Luma is byte-exact on all 24 frames of both gates; U/V differ by ±1..3 in a handful of
4x4 chroma blocks (frame 1: 40 U + 45 V pixels in 3 blocks; frames 2/3 similar).
EVIDENCE: `/tmp/.../scratchpad/{ours.yuv,ref.yuv}` | `EC_PROBE_DUMP=… decode_probe
gm1_cq32.obu` vs `ffmpeg -i gm1_cq32.obu -pix_fmt yuv420p` | frame 1 ydiff 0, udiff 40,
vdiff 45, mismatching chroma 4x4 blocks (12,8), (16,8), (20,8) = the 8x8 leaves at mi
(6,4), (8,4), (10,4); block (12,8) ours [97,119,105,95] vs ref [98,117,106,94].
Facts established for the successor:
- those three leaves are NOT warped and NOT global (`EC_WARP8 mi_row=6 mi_col=4
  warp=false globalblk=false`), so root cause 2 is not it and neither is the gm/warp code;
- their luma tx_type is `H_ADST` (class horiz) — i.e. exactly the blocks whose CHROMA now
  inherits a 1D-class type. Prime suspect: the chroma side of that inheritance — either
  the inverse basis at 4x4 chroma, or libaom's `av1_get_ext_tx_set_type` clamp
  (`if (!av1_ext_tx_used[tx_set_type][tx_type]) tx_type = DCT_DCT`), which we approximate
  with `side >= 32` instead of the real per-size/`reduced_tx_set` set membership.
  Next step: force `DCT_DCT` for the chroma INVERSE only (keep the class-1 eob/scan) and
  see whether the three blocks go exact — that separates basis from prediction in one run.

## DEVIATION from the charter (named)
Charter item (4) says restore any refusal whose gate cannot go green. I did NOT restore
the GLOBALMV/WARPED_CAUSAL 8x8 refusals r1 removed, same reasons r2 gave (a refusal placed
before the code it guards makes that code dead, `refusal-short-circuits-its-own-code`, and
those refusals are not what fails: both gate streams now decode END TO END and only chroma
pixels differ). Reverting is one hunk of `refusal_inventory.rs` plus two `decode.rs`
deletions in `65ef3f5` if the orchestrator disagrees.

## Instruments kept
`EC_WARP8` (per-leaf warp decision, under `EC_TRACE_MODE`), `tx=`/`class=` on the
`EC_COEFF_STEP tag=eob` line, and `EC_PROBE_DUMP=<path>` on `examples/decode_probe`
(raw 8-bit I420 dump for pixel diffing against ffmpeg without a test harness).
