# lane-troykf r1 handoff

Tip: see `git log -2`. Base `lane-sb128b` e8e64a6. Both root causes FIXED and the
lane is GREEN on its own gate; the only thing still owed is the final full-suite
number (the unit was still running at the turn cap, 318/~404 tests logged, zero
`FAILED` lines so far). Log: `$HOME/.cache/troykf-suite-r1.log`.

## Method (reusable)
`$HOME/.cache/troykf-work/one.sh <ss>` — extracts 2 s with `ffmpeg -ss <ss> -c:v copy
-f obu`, truncates to ONE frame with `trunc.py`, decodes under
`systemd-run --scope -p MemoryMax=6G` with `EC_PROBE_OUT16`, sha256s the output,
decodes the same truncated file with `ffmpeg -pix_fmt yuv420p10le`, and prints
per-plane diff count + first differing sample + bounding box.
`$HOME/.cache/troykf-work/stage.sh <ss>` — the same file through
`EC_AV1_PREFILT_DUMP16` / `EC_AV1_POSTDEBLOCK_DUMP16` on BOTH decoders.
`$HOME/.cache/troykf-work/sweep.sh <lavfi-src> <cq> <depth>` (env `MP=` for
max-partition) — aomenc recipe probe printing `troy_chroma:` counters.

## Per-key-frame findings (1920x792 10-bit AV1, 12 seek points)

All measurements are of decode-order frame 0 of a 1-frame truncation.

| ss | first differing sample (before) | extent | stage | verdict |
|----|--------------------------------|--------|-------|---------|
| 0    | none | — | — | exact before and after |
| 900  | U (r103,c304) | U rows 103..395 cols 304..479, V rows 99..383; luma exact | prefilter | fixed, now 0 |
| 1800 | U (r19,c538) | U rows 19..313 cols 311..732; luma exact | prefilter | fixed, now 0 |
| 2700 | U (r48,c468) | ONE 8x16-luma chroma pair, chroma rows 48..79 cols 464..492 | prefilter (467 diffs) vs postdeblock (499) — reconstruction, not a filter | fixed, now 0 |
| 3600 | U (r151,c469) | U rows 151..395 cols 468..959; luma exact | prefilter | fixed, now 0 |
| 4500 | none | — | — | exact before and after |
| 5400 | U (r164,c154) | U rows 164..168 cols 153..159 (one 8x4 chroma block) + V (r152,c278) | prefilter | fixed, now 0 |
| 6300 | U (r148,c545) | U rows 148..395; V rows 148..395 cols 84..903 | prefilter | fixed, now 0 |
| 7200 | U (r24,c561) | U rows 24..348 cols 210..959 | prefilter | fixed, now 0 |
| 9900 | U (r8,c500) | U rows 8..395 cols 497..959 | prefilter | fixed, now 0 |
| 8100 | frame 0 REFUSES (128-root non-SPLIT) | — | — | lane-sb128c owns it |
| 9000 | frame 0 REFUSES (128-root non-SPLIT) | — | — | lane-sb128c owns it |

LUMA WAS BIT-EXACT IN EVERY ROW, before and after. That single fact exonerated
the entropy stream at the first measurement and is why no range ladder was ever
needed.

## Blocks found, and the symbol/prediction comparison

* ss=2700, chroma (48,468) = luma (96,936) = mi(24,234). aomdec `EC_TRACE_MODE`:
  `EC_IMODE mi_row=24 mi_col=232..235 bsize=16` — four `BLOCK_4X16`
  (`PARTITION_VERT_4`); `mi_col=235` is the chroma reference and prints
  `uv_mode=13 skip=1`. Our chroma there was plain DC.
* ss=5400, chroma (164,152) = luma (328,608) = mi(82,76). aomdec: `mi_row=80..83
  mi_col=76 bsize=17` (`BLOCK_16X4`, `PARTITION_HORZ_4`), chroma reference
  `mi_row=83`, `uv_mode=5` (D113). aomdec `EC_PRED mi_row=83 mi_col=76 plane=1
  txw=8 txh=4 mode=5 p_angle=107 have_top=1 have_left=1 n_top=8 n_left=4 ft=0`,
  `EC_PREDOUT row0=415,431,453,471,467,442,408,397`.
  Ours (`EC_PRED=1`, `OUR_PRED x=152 y=164 bw=8 bh=4 mode=5 ad=-2`):
  `row0=[415,431,452,470,466,441,409,398]` — prediction, not residual.

## Fixes applied and their effect
1. `decode.rs:7719` + `:10528` + `:10891` — a skipped intra block still predicts
   with CfL (libaom `predict_and_reconstruct_intra_block` predicts every plane
   and guards only the residual on `skip_txfm`). ss=2700 499 -> 0.
2. `decode.rs:7714` — the 1:4 pair reads `smooth_uv_neighbour` at `pair_mi` (the
   chroma reference), not at the closing strip: libaom
   `get_intra_edge_filter_type` uses `chroma_above_mbmi`/`chroma_left_mbmi`. The
   wrong filter type flipped `av1_use_intra_edge_upsample` (`blk_wh = 8+4 = 12
   <= 16` upsamples for type 0, never for type 1). ss=5400 46 -> 0; every
   remaining seek point -> 0.
3. `gate_coverage.rs` — `enable-cfl-intra` retired from BOTH never-exercised
   lists, and `covers_both_depths` learned the `for depth in [8usize, 10]`
   spelling (a gate with a `yuv420p10le` arm was being classified 10-bit only).

## Hypotheses RULED OUT (do not re-spend rounds on these)
* Entropy desync / wrong CDF row: luma bit-exact on all ten frames.
* CDEF / loop restoration / deblock at a 128 superblock (the charter's leading
  suspects): the prefilter dump already carries the whole defect (ss=2700
  prefilter 467 diffs, postdeblock 499) and luma is exact through every filter.
* `delta_q` / `delta_lf` read at the 128 root: a wrong plane qindex would move
  the residual, not the prediction, and would not leave luma exact.
* Intra edge availability at 128 (`has_tr_128x128`/`has_bl_128x128`): `Reach` is
  two booleans computed from luma geometry, which libaom reuses for chroma; the
  ss=5400 `EC_PRED` line agrees (`n_tr=-1 n_bl=-1`).
* `Reach::of_rect(pw, ph, ...)` being luma-scaled while the chroma block is half
  the size: checked, correct — libaom computes availability once, in luma mi.

## Gate
`cargo test -p ec-av1 --lib a_real_aomenc_sb128_stream_whose_skipped_cfl -- --nocapture`
-> pixel-exact at 8-bit AND 10-bit, `directional_1to4_chroma_pairs=11` per arm,
`skipped_cfl=0`.

## Exact next step for a successor
1. `grep -E "^test result|FAILED" $HOME/.cache/troykf-suite-r1.log` — the suite
   was mid-run at the cap. If anything is red, it is a sibling of the two
   touched paths (grep `smooth_uv_neighbour` and `cfl_ac_q3`).
2. Defect 1 (skipped CfL) has NO synthetic gate. 20 aomenc recipes were swept
   (cq 40/44/46/50/52/55/58/62/63 x mandelbrot, blurred mandelbrot, noised
   mandelbrot, testsrc2, gradients, smptebars, cellauto, life, grey+noise;
   min-partition 4, max-partition 16/32/64, sb-size 128/64) and `skip_cfl`
   stayed 0 in every one. Either find a recipe that fires it (the counter is
   printed by the gate AND by `decode_probe`, so a hit is immediately visible)
   or commit a pinned single-frame stream fixture.
3. The two remaining Troy seek points (8100, 9000) need lane-sb128c's 128-root
   non-SPLIT partition; re-run the 12-point table after that lands.
