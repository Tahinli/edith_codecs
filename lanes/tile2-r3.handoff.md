# lane-tile2 r3 HANDOFF (fix COMMITTED, tip `f930263`)

## ROOT CAUSE FOUND AND FIXED
`decode_inter_block8`'s COMPOUND arm never called `resolve_interp_filter`, i.e.
never read libaom's `read_mb_interp_filter` (`decodemv.c`) `switchable_interp`
symbol(s) — it hard-coded `Regular` for both taps. Every 8x8 COMPOUND leaf of a
SWITCHABLE-filter frame desynced the entropy stream from the NEXT block on.
8x8 leaves only exist where the frame edge forces the partition down, so it
presented as "the bottom straddling mi row is wrong".

Fix (decode.rs, compound arm of `decode_inter_block8`, right after the
`compound_idx` read = libaom's order): neighbour filter ctx off `ref0` (same
shape as the single-ref arm), `av1_is_interp_needed`'s no-symbol cases
(`skip_mode`, or `GLOBAL_GLOBALMV` with BOTH refs' models non-TRANSLATION),
resolved `(h, v)` fed to all six `predict_compound_intermediate` calls,
`leaf_filter_syms` recorded for the neighbour band.

## The mv-stack question in the charter: ANSWERED, it was a symptom
`~/.cache/tile2-tmp/strad1.obu` (md5 12fb76d8910765657733604c4c70b77c), frame
with `EC_MODE mi_row=16 mi_col=32 rng=58738`:
- BEFORE: ours block(16,32) mode/mv/stack matched aomdec exactly (both mode=17
  ref0=1 ref1=5 mv0=(0,0), post-MV rng 37480 identical) but the NEXT block
  started at rng 42674 (ours) vs 38658 (aomdec) => the missing filter symbol.
  From there ours read PARTITION_HORZ at the 16x16 node mi(16,36) where aomdec
  read SPLIT, so ours had 16x8 blocks at 36/40/44 and aomdec 8x8s everywhere.
- aomdec `EC_STACK mi_row=16 mi_col=34 ref=8`: i0 (0,0) w=664, i1 (-1,-1) w=4 —
  ours has the SAME two candidates; the mv0 difference was NEARESTMV vs NEARMV
  chosen from a desynced bit, not a candidate order/weight defect.
- The r2 "stored motion field row 8 differs" finding is downstream of this:
  our row 8 faithfully stored our own (wrong) bottom-row mvs.
- AFTER: ours block(16,34) `EC_MODE rng=38658 mode=18 mv0=(-1,-1)` = aomdec.
- `EC_TRACE_MODE` now also dumps `EC_STACK` in the 8x8 leaf path (added here).

EVIDENCE: ~/.cache/tile2-tmp/{o5.y,a5.y} | decode_probe EC_PROBE_OUT vs aomdec --rawvideo on strad1.obu | `cmp` IDENTICAL (97920 B); before the fix, 164 luma px differed.

## Gate status
`a_frame_edge_straddling_band_decodes_pixel_exact`: STILL RED, but on a NEW arm
the 8-bit one used to mask. The r2 failure (`192x68 cq35 10bit=false
tile_cols=1 frame 1 Y: 164 px, row 59 col 146`) is GONE; now:
`192x68 cq35 frames=5 10bit=true tile_cols=1 frame 3 plane Y: 26 pixels differ,
first at row 61 col 185 (ours 704 vs ffmpeg 705) [edge32=[0,34,0,0,1,17,1,0]]`.
Arms (192x68 8/10-bit tile0, 68x192 8/10-bit tile0, 192x68 8-bit tile1) all pass.
Reproduce the residue: `bash ~/.cache/tile2-tmp/strad10.sh` (md5
a4399a4d5d1dc2e225f612a78a4a016a, hashed twice), then EC_PROBE_OUT16 vs aomdec.
NOT the filter application: with the symbol still read but the kernels forced
back to Regular (`EC_TILE2_FORCE_REG` ablation, since removed) that arm fails
EARLIER (frame 1, byte 62869) — the applied filters are right, the 26-px +-1 is
a separate reconstruction defect (10-bit + tiles).

## Unit result lines
- `tile2-gates-r3` (straddling + 5 multi-tile gates + mvstack::):
  `test result: FAILED. 37 passed; 1 failed; 1 ignored; 0 measured; 396 filtered out; finished in 52.59s`
  — the single failure is the 10-bit arm above; the var-tx multi-tile arm is
  still `ignored` (not retested this round).
- `tile2-suite-r2`: produced NO `test result:` line — it was stopped mid-run
  (see ledger constraint on concurrent units in one worktree).
- `tile2-suite-r3`: armed on `f930263`, log `$HOME/.cache/tile2-suite-r3.log`;
  NOT read before the turn cap. Read it first thing.

## Next
1. `$HOME/.cache/tile2-suite-r3.log` result line.
2. The 10-bit tile_cols=1 26-px residue (anchor above).
3. `#[ignore]`d var-tx multi-tile chroma arm: retest after (2).
