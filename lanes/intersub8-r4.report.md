# lane-intersub8 r4 — RED on the HORZ lift, GREEN on its root cause

## What changed
* `crates/ec-av1/src/decode.rs:15981+` — new `bsize_index(w,h)` (libaom `BLOCK_SIZES_ALL`
  order), `masked_compound_used_wh`, `wedge_used_wh`.
* `crates/ec-av1/src/decode.rs` (`decode_inter_block`, comp-type block) — `comp_group_idx`'s
  gate and the `compound_type` / `wedge_idx` CDF ROWS now come from the block's true
  `(write_w, write_h)`, not the caller's square `side`. A 16x8 masked-compound block used to
  read `BLOCK_16X16`'s row (index 6) where libaom reads `BLOCK_16X8`'s (index 5).
  Class: table-indexed-by-raw-size / CDF-row-held-constant.
* Same site — `COMPOUND_WEDGE` on a non-square block is refused by name (the wedge codebook
  here is built per square side only); listed in `refusal_inventory.rs`.
* `crates/ec-av1/src/decode.rs` — `EC_TRACE_MODE` gains `EC_CGI` / `EC_CIDX` / `EC_IF`
  (comp_group_idx, compound_idx, interp filter: ctx + value + range). These are the rungs
  that localized the defect; the oracle has no per-symbol print between `EC_MODE_MV` and
  `EC_MODE_VAL`, so ours had to be split.
* Sub-8x8 HORZ call site — still refused, refusal text rewritten to the measured residue,
  and `EC_AV1_SUB8_HORZ=1` decodes it anyway for the next round's bisect.

## Root cause (r3's open defect), proven
`~/.cache/intersub8-tmp/d1.obu` (192x128, 6 frames, 8-bit, transposed geq source, cq14 sp3,
`--enable-rect-partitions=1`), decode-order frame 5, `BLOCK_16X8` at mi(30,8):
| rung | ours before | ours after | aomdec |
|---|---|---|---|
| `EC_MODE` entry | 46505 | 46505 | 46505 |
| after mv (`EC_MODE_MV`) | 58288 | 58288 | 58288 |
| end of mode info (`EC_MODE_VAL`) | 54923 | **61760** | **61760** |
| next block `EC_MODE` mi(28,12) | 33919 | **60694** | **60694** |
The block reads `comp_group_idx=1` (ctx 0, correct — a ctx sweep 0..5 never reproduced the
oracle), then `compound_type` from the wrong CDF row. The 8x4 group to its left was innocent:
it only changed which value `comp_group_idx` took.

EVIDENCE: ~/.cache/intersub8-tmp/{o5b.log,o6.log,r4.log} | EC_TRACE_MODE ladder on d1.obu before/after the fix vs instrumented aomdec | end-of-mode-info rng 54923 -> 61760 == oracle, and `cmp` of the probe output vs `ffmpeg -pix_fmt yuv420p` is byte-identical for all 6 frames
EVIDENCE: ~/.cache/intersub8-tmp/sweep3.sh output (28 arms, transposed source, cq {8,12,14,16,20,26,32} x sp {3,6,9,12} x 8/10-bit, rect on) | full re-run under `systemd-run --scope` | mismatching arms with horz8x4>0: 4 (r3) -> 3; 8-bit cq14 sp3 (horz=4) and cq20 sp3 (horz=4) went MISMATCH -> EXACT

## Why the HORZ refusal did NOT lift (RED)
3 of 28 arms still mismatch with `horz8x4 > 0`: 8-bit cq32 sp9 (horz=3), 10-bit cq12 sp3
(horz=5), 10-bit cq16 sp3 (horz=2). On the 10-bit cq12 sp3 stream the ENTROPY IS EXACT — the
full `EC_MODE` range ladder is identical to aomdec's element for element (the only diff is two
extra `EC_MODE` prints for blocks aomdec's instrumentation does not print). The mismatch is
post-entropy: luma only, `|d| <= 2`, confined to x < 48 / y >= 112 (mi rows 28..31, mi cols
0..11), first appearing on decode-order frame 3 and propagating. Shape says filter (CDEF or
the deblock of the internal 8x4 edge), not reconstruction of the coefficients.
EVIDENCE: ~/.cache/intersub8-tmp/{a.txt,b.txt,m1.ours,m1.ref} | `diff` of the two EC_MODE ladders + per-frame python pixel diff | ladder diff = 2 extra lines, 0 range mismatches; frame 3: 125 differing luma samples, max |d| = 2, all in x<48 y>=112

## Gates
* `sub8x8_inter_split` (the r3 gate, SPLIT + VERT arms) unchanged — HORZ arm NOT added, since
  the shape is still refused.
* No new gate for the masked-compound fix: measured, the only streams that reach a rect
  masked-compound block are HORZ ones, which the refusal blocks. `deferred: gate for the rect
  compound_type/wedge_idx row — no non-HORZ recipe found that produces a rect block with
  comp_group_idx==1 — unblocked by either the HORZ lift or a `--enable-masked-comp` sweep on
  rect-heavy square-only content.`
* Full suite: unit `intersub8-suite-r4-1788331918.service` -> `$HOME/.cache/intersub8-suite-r4.log`,
  STILL RUNNING at hand-off (74 ok, 0 FAILED, no `test result:` line yet). NOT a green claim.
  r3's unit (`intersub8-suite-1788331066`) was stopped as charter-ordered; its log had 73 ok /
  0 FAILED and no `test result:` line at stop time.

## Film probe
0.4 s copy-extract at 0 s of the 2160p 10-bit HDR AV1 film yields only 1817 bytes (one OBU
set, no full frame); the probe returns counters (`horz8x4=0`) and no refusal, so the census's
start_s=0 refusal ("an inter partition below 8x8") is NOT reproduced by that extract.
`deferred: HG below-8x8 census probe — the bounded extract is too small to reach the block —
unblocked by extracting from a keyframe offset (census4 hunger4.tsv start_s column).`

## Residue
* fix-now(next round): the |d|<=2 luma band under HORZ. Reproduce with
  `EC_AV1_SUB8_HORZ=1` on `~/.cache/intersub8-tmp/t_10_12_3.obu` (regenerate:
  `./gen_t.sh 12 3 10 m1.obu --enable-rect-partitions=1`).
* accepted: `COMPOUND_WEDGE` on a rect block refused (rect wedge codebooks unimplemented).
