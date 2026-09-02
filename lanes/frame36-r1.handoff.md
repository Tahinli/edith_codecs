# lane-frame36 r1 HANDOFF (branch `lane-frame36`)

## Working stream (pinned prefix, not yet a repo fixture)
`~/.cache/frame36-tmp/n58.obu` -- `python3 <scratch>/census4/trunc.py ~/.cache/hg-0.obu 58 <out>`
of the 2 s head cut of the 10-bit 3840x1608 stream. 32126 bytes,
sha256 `7f3b060da5aa9c537633e760c4af71316237db7692f6ae0d459bd2c2dd867d37`,
md5 `1e93c12f622cd1fb0c1cd9485cd8932e`. 58 frame headers, 40 decoded frames,
37 output frames. `ffmpeg -i n58.obu -pix_fmt yuv420p10le -f rawvideo` = 685393920 B.

## What output frame 36 IS (EC_PROBE_HDR=1, a NEW rung added this round to decode_probe)
The charter's `EC_PROBE_HDR` did not exist; it does now (`decode_probe.rs`, one line per
frame header: type/show/showable/show_existing/refresh/refs/order hints/primary_ref/
q/seg/delta q,lf/lf levels+deltas/cdef bits/LR types/tx mode/ref_select/skip mode/warp/
reduced_tx/gm models/grain seed/lossless).

* OUTPUT frame 36 = header **57**, a `show_existing_frame` of slot 6, whose content is the
  hidden frame coded at header **52** (`show=false showable=true`, order_hint 36,
  primary_ref=1, refresh 0x40, base_q 61, lf_level [1,1,4,0]).
* The real first wrong frame is **decode-order frame 33 = header 49**: `show=false`,
  order_hint 64 (the next ARF), primary_ref=0, refresh 0x02, base_q 34,
  refs=[0,2,0,0,0,3,0], order_hints=[32,0,32,32,32,16,32], skip_mode_frame [1,6]
  (every other frame uses [1,5]). Fields NEW at this frame vs 0..48:
  **cdef_bits 0 -> 1** and **loop_restoration [None,None,None] -> [Switchable,Sgrproj,Switchable]**
  (both revert at header 50). First non-zero `lf_level` in the whole stream is header 47
  ([4,4,0,0], luma only); first non-zero CHROMA lf level is header 50 ([3,3,1,1]).
  size/superres/seg/intrabc/hp_mv/reduced_tx/gm/tx_mode never change; gm is Identity throughout.
* Decode-order frames 33,34,35,36 map to headers 49,50,51,52; the whole wall is
  "hidden ARF wrong -> everything that references it wrong" (class `gate-blind-to-hidden-frames`).

## Stage narrowing (all on n58.obu, both sides run under systemd-run MemoryMax=6G)
`EC_AV1_PREFILT_DUMP16` (ours) vs aomdec `EC_AV1_PREFILT_DUMP16` (the 8-bit
`EC_AV1_PREFILT_DUMP` **segfaults on this 10-bit stream** -- use the 16 variants;
aomdec has no POSTCDEF rung) at decode frame 33: prefilt already differs
(12222910 B of 18524160), i.e. **prediction/residual, not deblock/CDEF/LR/film grain**.

## First wrong pixel (BEFORE the fix)
Prefilt frame 33: Y (x=1920, y=64) ours 173 vs aomdec 170; U/V first at (960,32).
That is superblock (mi_row 16, mi_col 480), 64x64, PARTITION_NONE, intra, in an inter frame.

## Ladder result (EC_TRACE_PART/EC_TRACE_MODE_STEP ours vs aomdec EC_TRACE=1+EC_TRACE_MODE_STEP=1)
SB entry rng matches (36104 both). First diverging element:
`tx_depth` at mi(16,480) -- ours `val=0 ctx=1 rng=56472`, aomdec `val=0 ctx=0 cat=3 rng=34964`
(same value, wrong CDF ROW -> different range -> desync; class `wrong-alphabet-same-value`).
`EC_TXCTX` (print added to `tx_size_context_txfm` this round) shows why:
`left_txfm[16]=64` where libaom has the 64x16 strip's tx HEIGHT 16.
The preceding SB (16,464) is PARTITION_HORZ_4 = four 64x16 INTRA strips inside an inter frame.

## Root cause + fix (committed)
`decode_block_rect64` / `decode_block_rect4` (the 1:4 strip readers, which are ALSO the inter
frame's readers via `decode_intra_rect4_in_inter`, decode.rs ~7455) never published libaom's
`set_txfm_ctxs` bands, so the next block read `TXFM_CTX_INIT` (64) as the left transform height.
Fix = one call each to the existing `txfm_partition_update_rect(neighbours, at_mi, (tx_w,tx_h), (bw,bh))`
at their tails (decode.rs:9389 area and :9911 area).
Result on n58.obu: output frame 36 differing bytes **1992768 -> 765464**; decode-order
prefilt f33 12222910 -> 6446616, f34 4549085 -> 1005614, f35 -> 759498, f36 -> 763799.
Frames 0..32 and 37,38,39 stay byte-exact vs aomdec's own FINAL dump.

## EXACT NEXT STEP
The wall is NOT closed -- decode frame 33 (header 49) still differs. Redo the same ladder on the
NEW binary: `EC_TRACE_PART=1 EC_TRACE_MODE_STEP=1` ours vs `EC_TRACE=1 EC_TRACE_MODE_STEP=1`
aomdec, group lines into frames by the `EC_PART mi_row=0 mi_col=0 bsize=12` marker (ours now
emits 40 groups, aomdec 40; our SB-root print skips the last partial SB row -- compare by
`(mi_row,mi_col) -> rng`, our `ctx=` field is NOT aomdec's), find the first SB whose ENTRY rng
differs in frame 33 and dump every line of the SB BEFORE it. Then: pin the prefix as
`crates/ec-av1/fixtures/hg_frame36_witness.obu` (`git add -f`) with a gate over every plane of
every decode-order frame + a hard assert on `intra_rect4_in_inter` (this stream: 64x16=8 16x64=9),
update gate_coverage, run the suite as a systemd unit, and re-probe the full `hg-0.obu`.

## Artifacts (all under ~/.cache/frame36-tmp)
`n58.obu`, `hdr58.txt` (58 EC_PROBE_HDR lines), `ours/` + `aom/` (FINAL dumps, 40 frames each),
`opre/` + `apre/` (PREFILT16), `opd/` + `apd/` (POSTDEBLOCK16), `opart.txt`/`apart.txt`,
`omode.txt`/`amode.txt`, `osb464.txt`/`asb464.txt`, `otx.txt`.

EVIDENCE: ~/.cache/frame36-tmp/{o,r}.raw | release decode_probe + ffmpeg on the pinned 58-OBU prefix, diff16.py 3840x1608 | frames 0-35 0 bytes, frame 36 1992768 -> 765464 after the fix
EVIDENCE: ~/.cache/frame36-tmp/{opre,apre}/p.f33 | ours EC_AV1_PREFILT_DUMP16 vs aomdec EC_AV1_PREFILT_DUMP16 | 12222910 differing bytes pre-filter => not a post-filter defect
EVIDENCE: ~/.cache/frame36-tmp/{omode,amode}.txt | EC_TRACE_PART+EC_TRACE_MODE_STEP vs aomdec EC_TRACE+EC_TRACE_MODE_STEP | first diverging element tx_depth mi(16,480) ctx 1 vs 0, rng 56472 vs 34964
