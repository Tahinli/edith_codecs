# lane-sbrect10 r2 handoff

Branch `lane-sbrect10`, tip `2b9709c` (worktree
`/home/tahinli/Documents/Code/Rust/edith_codecs-sbrect10`).
Commits this round: `3a9e83e` (merge main 18bf7dc) + `2b9709c` (the fix + gate + instrumentation).

## STATUS: root cause FOUND and FIXED; pinned stream now byte-exact on all 6 probe arms.
## Only thing outstanding: the full suite result (armed, was at 340 tests / 0 failures).

## 1. Merge state (done, clean)
`git merge --no-commit main` (main = 18bf7dc: r14 a2e2e29 + sb128 5ea5ec8) auto-merged
decode.rs + stream.rs with no conflicts. Verified before committing:
- `git diff main -- crates/ec-av1/src/cdf.rs` EMPTY and `... refusal_inventory.rs` EMPTY, so the
  NZ_MAP / FILTER_INTRA rows and the refusal list are byte-identical to main.
- The only lines the merged tree removes vs main are the lane's own r1 hunks (4 `uv_mode_cfl`
  lines in decode.rs, the `[8u32]` arm + its comment in stream.rs). Main's deletions all kept.

## 2. Gate fixture state (done)
`a_real_aomenc_inter_frame_with_a_64x64_intra_block_reads_the_no_cfl_uv_alphabet` (stream.rs)
now uses the half-random source in BOTH orientations:

    horizontal: if(lt(X,128), 40+mod(floor((Y+N*sp)/32)*90,200), mod((X*7+Y*13+N*97)*31,256))
    vertical:   if(lt(Y,64),  40+mod(floor((X+N*sp)/32)*90,200), mod((X*7+Y*13+N*97)*31,256))

r1's flat `200` half made aomenc code the right-hand superblocks skip, so the 8-bit arm saw 0
intra-above-32x32 blocks (probed cq 45/52/58/61/63). Grid `[58, 52, 45, 61]` x 2 orientations x
2 motion steps x 2 depths unchanged; counters are read on decoded+compared attempts only, every
decode-order frame is compared on Y/U/V, a mismatch fails and never SKIPs.

## 3. The 10-bit defect: root cause, libaom line, fix
Pinned stream `$HOME/.cache/sbrect10/b58.obu`, sha256 hashed TWICE identical
`00b9253b308201118e27f348c67932a0006e3077ba7a9a401854c2ee7eb1dcd2`
(192x128, 6 frames, 10-bit cq58, `--sb-size=64 --min-partition-size=32 --max-partition-size=64
--enable-rect-partitions=1 --enable-1to4-partitions=1 --lag-in-frames=0 --cpu-used=0`;
regenerate with `bash $HOME/.cache/sbrect10/probe.sh "<geq>" 58 10`).

How it was localised (each step ruled a layer out):
- First mismatch DECODE-ORDER frame 2, plane Y only (U/V exact), 8252 samples; per-64x64-SB map
  row0 `[0,0,4092]`, row1 `[0,64,4096]` = the whole RIGHT superblock column + deblock bleed.
- NOT an entropy desync: our `EC_TRACE_COEFF` all_zero ladder vs instrumented aomdec's is
  identical for all 180 TUs of the whole stream (`diff` empty), and blocks decoded AFTER the
  wrong ones (mi(16,0)) are pixel-exact.
- NOT a clamp: an `EC_DBG_CLAMP` probe on the dequant clamp `+-(1 << (7 + bd))` and on every
  `clamp_range` in transform.rs fired ZERO times on this stream.
- `EC_TRACE_MODE` mv diff, decode-order frame 2: aomdec mi(0,32) mode=24 ref0=1 ref1=4
  mv0=(128,-16) / ours (-1,2); mi(0,40), mi(8,32), mi(8,40), mi(16,40) all (128,-16) vs (-1,2);
  mi(16,32) (-176,256) vs (-241,264); mi(24,32) (-128,56) vs (-193,64). The
  NEAREST_NEARESTMV blocks take stack[0] directly, so libaom's stack[0].this_mv is (128,-16)
  where ours is (0,0), and the NEWMV blocks are off by the same predictor delta.
- Our stack for mi(0,32) = two entries `this=(0,0) comp=(0,0) w=2` -- the signature of the
  compound EXTENSION pass appending comp_list[0] and comp_list[1] into an empty stack.
- Frame 2's temporal field really is empty and that is CORRECT (new `EC_TPL_FIELD` occupancy
  dump under EC_TRACE_TPL: oh=1,2 all `.`, oh=3,4,5 all `#`; frame 2's LAST projection is
  suppressed by `is_lst_overlay` and its LAST2 is the key frame). So the candidate is spatial.

ROOT CAUSE: `combine_compound_candidates` (crates/ec-av1/src/mvstack.rs:1482) filled the
`comp_list` tail slots -- the ones left after `ref_id` then `ref_diff` -- with `(0, 0)`.
libaom fills them with that side's GLOBAL MOTION vector, `setup_ref_mv_list`,
`av1/common/mvref_common.c:729`:

    for (; comp_idx < MAX_MV_REF_CANDIDATES; ++comp_idx)
      comp_list[comp_idx][idx] = gm_mv_candidates[idx];

Zero-filling was right only under IDENTITY global motion (the module doc's original reduction);
aomenc has `--enable-global-motion` ON by default. A compound block with no usable neighbour
pair (frame top row -> no above, 64x64 INTRA block to its left, empty temporal field) therefore
took a `(0,0)` predictor where libaom took the frame's translation.

FIX: `combine_compound_candidates` takes `gm_mv_candidates: [(i32,i32); 2]` and uses it as the
per-side fill; the call site passes `[gm_mv(gm, ref_frame.0), gm_mv(gm, ref_frame.1)]`.

SIBLING SWEEP (same shape): every `gm_mv_candidates` use in mvref_common.c was walked. The
single-ref tail fill (`:787-788`) is already ported as `gm_fallback` at mvstack.rs:1099-1100;
the `is_gm_block` substitutions (`:92`, `:117`) already take `gm` through `scan_*_compound`; the
add_tpl_ref_mv GLOBALMV_OFFSET compares (`:379-410`) already use `gm_mv(gm, ...)` on both sides.
This was the only zero-filled site in the crate.

## 4. Is the pinned stream exact now? YES
`probe.sh "<half-random geq>" <cq> <depth>` after the fix, pixel diffs vs
`ffmpeg -pix_fmt yuv420p10le` / `yuv420p`, and `>32` intra-in-inter hit counts:

| depth | cq 52 | cq 58 | cq 61 |
|---|---|---|---|
| 8-bit  | 5 hits, 0 diffs | 2 hits, 0 diffs | 3 hits, 0 diffs |
| 10-bit | 6 hits, 0 diffs | 7 hits, 0 diffs | 7 hits, 0 diffs |

Before the fix the 10-bit arms were 32890 (cq58) and 39907 (cq61) wrong samples.

## 5. NEXT STEP (the only thing left)
Read `$HOME/.cache/sbrect10-suite-r2.log` -- the suite is armed as a user systemd unit
(`sbrect10-suite-r2-1788325070.service`, MemoryMax=10G, `cargo test -p ec-av1 --lib -j3`,
EC_AV1_REQUIRE_AOMENC=1, CARGO_TARGET_DIR=$HOME/.cache/cargo-target-sbrect10). At handoff it had
reached 340 tests with no FAILED line. Check with a single
`grep -E "^test result|FAILED" $HOME/.cache/sbrect10-suite-r2.log`; if a gate is red, re-run that
gate alone before touching code (another lane rebuilding the shared aomdec shows up as
"Os code 13 PermissionDenied"). Then fill the N/0 totals into `lanes/sbrect10-r2.report.md`
(section 4 is otherwise complete) and the lane is done.

Nothing else is in flight; the worktree is clean at 2b9709c.
