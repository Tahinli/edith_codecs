# lane-cdef r1 -- HANDOFF (2 CDEF defects rooted, both fixed, gate GREEN; one residue open)

Base: main 48b35ab. Suite unit `cdef-suite-1788330834.service` -> `$HOME/.cache/cdef-suite-r1.log`
(started at the turn cap; FIRST THING NEXT ROUND: `grep -E "^test result|FAILED" $HOME/.cache/cdef-suite-r1.log`).

## Defect A -- per-8x8 CDEF skip band never written by the 8x8 inter leaf (charter hypothesis (a), variant)

* Stream: `~/.cache/cdef-tmp/a0_8.obu`, sha256 `aa40f17f6e49eca2bb8c9d9ab99a175312134bdcf45c39b8a76b6757d8536751`
  (hashed twice, identical), `~/.cache/cdef-tmp/gen.sh 8 3 8 a0_8.obu` = lane-intersub8's recipe,
  192x128, 6 frames, `--min-partition-size=4`, cq 8, 8-bit.
* Stage attribution: reconstruction and post-deblock are EXACT (`EC_AV1_POSTDEBLOCK_DUMP` ours vs
  the instrumented aomdec: 0/24576 luma on decode-order frame 3); the 3 samples enter at CDEF.
  NOTE: aomdec's `EC_AV1_PREFILT_DUMP` is ALREADY POST-DEBLOCK on this build (r_pre.fN == r_pd.fN
  for 5 of 6 frames) -- ours is genuinely pre-deblock, so the two rungs are NOT the same stage.
* Root cause: `decode_inter_block8` (decode.rs, the leaf path `--min-partition-size=4` picks for a
  16x16 group) wrote `record_mi` + `fill_lf_grid` at both exits but NEVER `fill_skip_grid`. The
  bottom-right 16x16 of decode-order frame 3 was therefore judged by the PREVIOUS frame's flags:
  our 8x8 units at mi(28,46)/(30,46) read skip=false and were filtered, while libaom's
  `is_8x8_block_skip` (cdef.c:29-38, called from `av1_cdef_compute_sb_list` cdef.c:40-71) excludes
  an all-skip 8x8 from the dlist. Proof: a python model of `cdef_block.c` reproduces the reference
  for all 16 units of the last column at (sec=1,damp=3) EXCEPT those two, where only "unfiltered"
  matches; frame-3 fill log showed 0 of 95 skip fills covering mi(28,44)+16x16.
* Fix: `neighbours.fill_skip_grid(leaf_mi, 2, skip)` at both `decode_inter_block8` exits (compound
  early return + fall-through), each bumping `INTER8_SKIP_BAND_HITS`.
* Before/after on the pinned stream: frames 3 and 4 had 3 luma samples |d|=1 each
  ((189,116/120/124) and (186,116/120/124)); after, all 6 frames pixel-exact on Y/U/V.

## Defect B -- CDEF direction search clamped at the crop edge instead of reading CDEF_VERY_LARGE

* Stream: `~/.cache/golomb-tmp/s68.obu` (68x192, 5 frames, 8-bit, cq 35) -- lane-golomb r9's
  deferred "straddle band".
* Root cause: `apply_cdef`'s `cdef_find_dir` closure clamped to `true_w-1`/`true_h-1`, but libaom
  runs `cdef_find_dir` over `cdef_prepare_fb`'s buffer (cdef.c:249-256 `frame_boundary[RIGHT]`
  fill, cdef_block.c:296-320), whose columns/rows past the mi-rounded extent are CDEF_VERY_LARGE.
  With odd `mi_cols` (68 px -> 17 mi) the last 8x8 straddles, so var/`adjust_strength` differed.
* Fix: the direction search now reads the SAME padded `sample_y` the taps use (one line).
* Before/after: frame 1 luma 145 diffs -> **0**; all 5 frames luma exact.
* RESIDUE (open, NOT fixed): the same stream still differs in CHROMA from frame 1 on
  (U/V 194/401/466/551 samples, |d| up to 6, spread over ~37 chroma 4x4 units, frame 0 exact).
  Not attributable yet: `EC_AV1_DEBUG_SKIP_CDEF=1` cannot bisect inter frames (it also breaks the
  reference frames), so the postdeblock rung is only valid for frame 0 here. Next step: dump
  per-4x4 chroma CDEF params (uv pri/sec, damping-1, shared luma dir) for frame 1's first
  differing unit and model it against `cdef_block.c` the way defect A was modelled
  (`~/.cache/cdef-tmp/model.py` is a complete python port of cdef_block.c + find_dir + adjust).

## Gate
`stream.rs::a_real_aomenc_stream_with_cdef_and_sub16_inter_leaves_decodes_pixel_exact` -- real
aomenc, `--min-partition-size=4 --enable-cdef=1` (last flag wins), 5 arms (8-bit cq 8/10/12,
10-bit cq 8/12), every decode-order frame compared on Y/U/V, hard asserts
`decode::cdef_skipped_units()` (an 8x8 unit really excluded by the all-skip rule) and
`decode::inter8_skip_band_hits()` (the leaf that used to write nothing) both grew per compared
attempt. 2 of 5 arms fire; the other 3 hit UNRELATED open refusals (non-DC chroma mode on an 8x8
inter leaf; inter partition below 8x8) -- widening to 10-bit needs those lifted first.
`cargo test -p ec-av1 --lib cdef_and_sub16` -> ok, 1 passed / 0 failed.

## Other
* `dump_stage`/`dump_stage16` wrote `.f0` for EVERY frame (each overwrote the last), so
  `EC_AV1_POSTDEBLOCK_DUMP`/`POSTCDEF_DUMP` were unusable for per-frame bisection. Now indexed
  per env var (`dump_stage_idx`), matching aomdec's own `.fN`.
* New rung `EC_AV1_CDEF_DBG=1`: one line per filtered 8x8 luma unit
  (`EC_CDEF mi_r= mi_c= x= y= sidx= dir= var= t= pri= sec= damp=`).
* Class sweep for "per-8x8 map read at the top-left mi only under sub-8x8 partitions": the skip
  grid is written by every OTHER leaf path (grep `fill_skip_grid`), `decode_inter_block8` was the
  only writer missing; `is_skip_txfm` already gathers all four cells; the deblock/tx grid has a
  `fill_lf_grid` at every one of those sites including both `decode_inter_block8` exits.
* Not done: `--tile-columns=1` arm on the new gate (COMMON neighbour-map rule) -- deferred.
