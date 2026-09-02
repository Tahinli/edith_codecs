# lane-sbrect10 r2 report

Branch `lane-sbrect10`, worktree `/home/tahinli/Documents/Code/Rust/edith_codecs-sbrect10`.
Base: r1 tip 6fba6a6 + `main` 18bf7dc (sb128 5ea5ec8 + r14 a2e2e29) merged.

## 1. Merge (step 1)
`git merge --no-commit main` -> clean auto-merge (decode.rs, stream.rs), commit `3a9e83e`.
- Diffstat 12 files, +1392/-147 (all of it main's; the lane contributed nothing to those files
  beyond its own r1 hunks).
- `git diff main -- crates/ec-av1/src/cdf.rs` and `... refusal_inventory.rs` are BOTH EMPTY, so
  the NZ_MAP / FILTER_INTRA tables and the refusal list are byte-identical to main -- nothing to
  reconcile.
- The only lines the merged tree removes relative to main are the lane's own 4 r1 CFL lines in
  `decode.rs` (`cdfs.uv_mode_cfl[mode]` + its `cfl_alphas` arm) and the r1 comment/`[8u32]` arm in
  `stream.rs`. Main's deletions are all kept.

## 2. Gate source swapped to the half-random fixture (step 2)
`crates/ec-av1/src/stream.rs` (gate
`a_real_aomenc_inter_frame_with_a_64x64_intra_block_reads_the_no_cfl_uv_alphabet`): the flat
`200` half of r1's source made aomenc code the right-hand superblocks skip, so at 8 bit no
`>32` intra-in-inter block ever appeared. Both orientations now carry a deterministic
pseudo-noise half:

    horizontal: if(lt(X,128), 40+mod(floor((Y+N*sp)/32)*90,200), mod((X*7+Y*13+N*97)*31,256))
    vertical:   if(lt(Y,64),  40+mod(floor((X+N*sp)/32)*90,200), mod((X*7+Y*13+N*97)*31,256))

Probe grid, `$HOME/.cache/sbrect10/probe.sh` (counts `>32` intra-in-inter blocks via
`EC_AV1_TELL`, and pixel diffs vs `ffmpeg -pix_fmt yuv420p10le`/`yuv420p`), AFTER the r2 fix:

| depth | cq 52 | cq 58 | cq 61 |
|---|---|---|---|
| 8-bit  | 5 hits, 0 diffs | 2 hits, 0 diffs | 3 hits, 0 diffs |
| 10-bit | 6 hits, 0 diffs | 7 hits, 0 diffs | 7 hits, 0 diffs |

Both depth arms therefore carry >=1 no-CFL intra hit per compared attempt; per-attempt counters
are read on decoded+compared attempts only, every decode-order frame is compared on Y/U/V, and a
mismatch fails (never SKIPs).

## 3. The SECOND defect this fixture exposed -- root cause and fix (step 3)

### Pinned stream
`$HOME/.cache/sbrect10/b58.obu` (192x128, 6 frames, 10-bit, cq 58, `--sb-size=64
--min-partition-size=32 --max-partition-size=64 --enable-rect-partitions=1
--enable-1to4-partitions=1 --lag-in-frames=0 --cpu-used=0`), sha256 hashed twice, identical:
`00b9253b308201118e27f348c67932a0006e3077ba7a9a401854c2ee7eb1dcd2`.

### Localisation
- First mismatching frame: DECODE-ORDER 2 (display == decode order), plane Y only, 8252 samples;
  U and V exact; frames 0 and 1 exact. Per-64x64-SB wrong-pixel map: row0 `[0, 0, 4092]`,
  row1 `[0, 64, 4096]` -- i.e. the whole right superblock column, plus 64 deblock-bleed pixels.
- NOT an entropy desync: the 180-TU `all_zero` range ladder is IDENTICAL between our
  `EC_TRACE_COEFF` and instrumented `aomdec`'s (`diff a.az o.az` empty over the WHOLE stream),
  and blocks decoded AFTER the wrong ones (mi(16,0)) are pixel-exact.
- NOT a clamp: an `EC_DBG_CLAMP` probe on both the dequant clamp
  (`+-(1 << (7 + bd))`) and every `clamp_range` in `transform.rs` fired ZERO times on this stream.
- The divergence is in the MV VALUES. `EC_TRACE_MODE` diff, decode-order frame 2:

      aomdec  mi(0,32) mode=24 ref0=1 ref1=4 mv0=(128,-16)   ours mv0=(-1,2)
      aomdec  mi(0,40) mode=17 ref0=1 ref1=4 mv0=(128,-16)   ours mv0=(-1,2)
      aomdec  mi(8,32) / mi(8,40) / mi(16,40): same (128,-16) vs (-1,2)
      aomdec  mi(16,32) mode=24 mv0=(-176,256)               ours (-241,264)
      aomdec  mi(24,32) mode=16 mv0=(-128,56)                ours (-193,64)

  The NEAREST_NEARESTMV blocks take stack[0] directly, so libaom's stack[0].this_mv is
  `(128,-16)` where ours is `(0,0)`; the NEWMV/NEW_NEWMV blocks are off by exactly the same
  predictor delta.
- Our stack for mi(0,32): two entries, `this=(0,0) comp=(0,0) w=2` -- the signature of libaom's
  compound EXTENSION pass appending `comp_list[0]` and `comp_list[1]` into an empty stack.
- The temporal field is genuinely empty for that frame and this is CORRECT: a new
  `EC_TRACE_TPL` field dump (`EC_TPL_FIELD`) shows oh=1 and oh=2 all-`.` and oh=3 onward all-`#`;
  frame 2's LAST projection is suppressed by `is_lst_overlay` and its LAST2 is the key frame.

### Root cause
`crates/ec-av1/src/mvstack.rs:1482` `combine_compound_candidates` filled the `comp_list` tail
slots (the ones left over after `ref_id` then `ref_diff`) with `(0, 0)`. libaom fills them with
that side's GLOBAL MOTION vector -- `setup_ref_mv_list`, `av1/common/mvref_common.c:729`:

    for (; comp_idx < MAX_MV_REF_CANDIDATES; ++comp_idx)
      comp_list[comp_idx][idx] = gm_mv_candidates[idx];

The zero fill was only right under IDENTITY global motion, which the module's original reduction
assumed; aomenc has `--enable-global-motion` ON by default. A compound block with no usable
neighbour pair (frame top row -> no above, 64x64 INTRA block to its left, empty temporal field)
therefore took a `(0, 0)` predictor where libaom took the frame's translation.

Class: `parsed then discarded` / reduction-assumption -- global motion is parsed and used by the
single-ref path (`gm_fallback`, mvstack.rs:1099-1100) but was dropped on the compound tail.

### Fix
`crates/ec-av1/src/mvstack.rs`: `combine_compound_candidates` now takes
`gm_mv_candidates: [(i32,i32); 2]` and uses it as the per-side fill; the call site passes
`[gm_mv(gm, ref_frame.0), gm_mv(gm, ref_frame.1)]`.

### Sibling sweep (same shape, whole crate)
Every `gm_mv_candidates` use in `mvref_common.c` was walked. The single-reference tail fill
(`mvref_common.c:787-788`, `mv_ref_list[idx] = gm_mv_candidates[0]`) is already ported
(`mvstack.rs:1099-1100` `gm_fallback`); the scan-row/col/corner `is_gm_block` substitutions
(`:92`, `:117`) already take `gm` through `scan_*_compound`; the `add_tpl_ref_mv`
`GLOBALMV_OFFSET` compares (`:379-410`) already use `gm_mv(gm, ...)` on both sides. This was the
only zero-filled site.

## 4. Other change
`crates/ec-av1/src/decode.rs` `dump_stage16`: the `EC_AV1_PREFILT_DUMP16` dump wrote a fixed
`<prefix>.f0` every frame, so only the LAST frame survived and a mid-sequence divergence could
not be read out at all. It is now indexed by decode-order picture like `EC_AV1_PREFILT_DUMP`.
`crates/ec-av1/src/motion_field.rs`: `setup_motion_field` prints an `EC_TPL_FIELD` occupancy map
under the existing `EC_TRACE_TPL` rung (this is what proved frame 2's empty temporal field).

## 5. Refusals
None lifted this round (r1 lifted none either; this lane's work is two silent-miscode fixes).
`refusal_inventory.rs` is byte-identical to main.

## Suite (step 4) -- RED
`test result: FAILED. 368 passed; 3 failed; 32 ignored; finished in 1049.95s`
(`$HOME/.cache/sbrect10-suite-r2.log`). film_grain is a shared-`fixtures`-symlink flake and
passes alone; the other two are open and triaged in `lanes/sbrect10-r2.handoff.md` section 5c.
Section 2's probe table proves only the 6 probed points -- it does NOT prove the gate's own
counter, which still reads 0 on the 8-bit arm.

## EVIDENCE
EVIDENCE: $HOME/.cache/sbrect10/b58.obu (sha256 00b9253b...dcd2, hashed twice) | probe.sh half-random geq, 6 arms (8/10-bit x cq 52/58/61), decode + compare vs ffmpeg | pixel diffs 0/0/0/0/0/0 (was 0/32890/39907 on the 10-bit arms), >32 intra-in-inter hits 5/2/3 (8-bit) and 6/7/7 (10-bit)
EVIDENCE: $HOME/.cache/sbrect10/{a.az,o.az} | EC_TRACE_COEFF all_zero ladder, instrumented aomdec vs ours, whole stream | 180 TUs, `diff` empty -> entropy in sync, defect is reconstruction-side
EVIDENCE: $HOME/.cache/sbrect10/{a.mode,o.mode} | EC_TRACE_MODE mv0 diff, decode-order frame 2 | aomdec mi(0,32) mv0=(128,-16), ours (-1,2); 6 blocks diverge, all in the right superblock column
EVIDENCE: $HOME/.cache/sbrect10/o.tplf | EC_TPL_FIELD occupancy dump, all 5 inter frames | oh=1,2 empty / oh=3,4,5 full -> the empty temporal field at oh=2 is correct, not the defect
