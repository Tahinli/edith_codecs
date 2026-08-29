# lane-wii r2 charter — CONTINUATION (r1 was 100% recon, zero edits)

Execute the r1 HANDOFF plan verbatim, with ONE CORRECTION, in worktree
/home/tahinli/Documents/Code/Rust/edith_codecs-wii (lane-wii @ 6f4ca37).
Build/test ONLY: `env CARGO_TARGET_DIR=$HOME/.cache/cargo-target-wii cargo test -p ec-av1 --release --lib <name> -- --nocapture`
FOREGROUND, `nice -n 19`, `CARGO_BUILD_JOBS=4`. BUDGET ~55 calls; commit compiling
WIP + report by call 40. Do NOT redo recon the handoff already grounded.

## CORRECTION (r1 handoff error — this would desync)
The wedge index is NOT `dec.literal(4)`. It is an ADAPTING CDF symbol, exactly
like the COMPOUND_WEDGE path at decode.rs:5418:
  `let wedge_index = dec.symbol(&mut cdfs.wedge_idx[wedge_bsize]);`
(8x8 leaf path uses `cdfs.wedge_idx[3]`, see decode.rs:6927.) NO sign symbol is
read for interintra; sign is fixed 0 (libaom blockd.h INTERINTRA_WEDGE_SIGN 0).
The `wedge_bsize` index for 16/32 already exists at the wedge_interintra read
site (decode.rs:6075: `let wedge_bsize = if side == 16 { 6 } else { 9 };`) —
verify wedge_idx uses the SAME bsize indexing as wedge_interintra there (both
are [BLOCK_SIZES_ALL]-indexed in libaom; our cdfs.wedge_idx is [;22] full-size,
our wedge_interintra was remapped — check how 5418 computes ITS wedge_bsize and
copy that).

## r1 HANDOFF plan (verified findings, follow as written)
- Blend semantics (reconinter.c:1059-1076 combine_interintra): wedge branch =
  aom_blend_a64_mask(comppred, intrapred, interpred, mask,
  mask_stride=block_size_wide[bsize], bw, bh, subw, subh) — mask weights INTRA
  (src0), same polarity/rounding as the existing smooth arm
  `(m*intra + (64-m)*inter + 32)>>6` in interintra_blend (decode.rs:4660-4673).
  subw/subh: luma 0; 4:2:0 chroma 1 → chroma m = (m00+m10+m01+m11+2)>>2.
- Codebook API: crate::wedge::wedge_masks().codebook(side).mask(0, wedge_index)
  -> &'static [u8], row stride = cb.bw = luma side.
- Edit sites (tag #EE47; line numbers valid BEFORE first edit):
  (a) WII_HITS static+accessor after decode.rs:227 (copy MASKED_COMPOUND_HITS
      shape 205-215);
  (b) interintra_blend sig at 4628: insert `wedge: Option<(&'static [u8], usize)>`
      before `pred`; mask selection 4663-4668 →
      `match wedge { Some((mask, ms)) if ms == side => mask[i*side+j],
        Some((mask, ms)) => { let t = 2*i*ms + 2*j;
          ((u32::from(mask[t]) + u32::from(mask[t+1]) + u32::from(mask[t+ms])
            + u32::from(mask[t+ms+1]) + 2) >> 2) as u8 },
        None => match ii_mode { ...existing... } }`;
  (c) 16/32 path: `let mut wedge_mask: Option<(&'static [u8], usize)> = None;`
      after the interintra_mode decl at 6064; refusal 6076-6081 → read
      wedge_index per the CORRECTION, WII_HITS.fetch_add, set wedge_mask from
      the codebook; blend calls 6401-6403 pass wedge_mask to all three planes
      (chroma subsampling handled inside blend via ms != side);
  (d) 8x8 leaf path: re-read decode.rs:7290-7330 first (r1 never saw
      7305-7319); same shape at the 7313-7319 refusal, wedge_idx[3], decl in
      that scope, blend calls 7461-7463.
- Gates in order: cargo check first; then scoped
  `cargo test -p ec-av1 --release --lib interintra -- --nocapture`; then the
  15-pin default list (pinned_warp); then full lib. WIP COMMIT after every
  green milestone.
- Gate recipe: extend stream.rs a_real_aomenc_stream_with_interintra_decodes_
  pixel_exact (~3460) with a wedge variant copying the aomenc flag tail
  (~3595-3598) + --enable-interintra-wedge=1, fallback --enable-masked-comp=0,
  mandelbrot/gradient content; refusal string FORBIDDEN once fired; soft-skip
  zero-hit runs; hammer 6x with EC_AV1_GATE_DUMP self-pin
  (/tmp/claude-1000/wii-flake-N.obu). Mismatch = pin + report recon-diff
  location, do NOT guess-fix.
- Report lanes/wii-r1.report.md: verdict FIRST line, evidence per claim, note
  both charter corrections (r1's SIGN=1 error AND r1's literal(4) error).
