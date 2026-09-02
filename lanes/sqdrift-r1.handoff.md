# lane-sqdrift r1 -> r2 handoff

Branch `lane-sqdrift` @ 6edd5af (off main 85887c7). Worktree
`/home/tahinli/Documents/Code/Rust/edith_codecs-sqdrift`, `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-sqdrift`.

## Do NOT redo any of this (all measured, see lanes/sqdrift-r1.report.md for the EVIDENCE lines)
* Stream: `/home/tahinli/.cache/sqdrift/gen.sh` -> `s.obu`, sha256
  `c6af4fb4ebdc3d74dcfa0c945c0ef2d5e1e3a0902891d9e0a97a5608776b5d55`, hashed twice.
* Frames 0-3 byte-exact pre-filter AND post-filter (both dumps). No poisoned reference.
* Frame 4 damage = exactly the last SB, mi(16,32), PARTITION_NONE at 64x64, INTRA in an
  inter frame (aomdec prints neither EC_MODE nor EC_IMODE for it).
* We enter that block IN SYNC: our `pre_rng=33730` == aomdec `EC_PART ... rng=33730`.
* TX_64X64 dequant + inverse transform EXONERATED against real libaom (0/4096).
* The stream contains no rect/AB/1:4 partition at all; main's 1:4-strip refusal on it is
  a phantom of this desync.
* aomdec's prediction there is none of DC/SMOOTH/SMOOTH_V/SMOOTH_H/PAETH computed from
  the true neighbours, so its coefficients differ too -> a SYMBOL diverges, not a predictor.

## The one open question
Which symbol between `partition_w64` and the end of that block. Ordered suspects:
1. `read_delta_q_params` -- delta_q IS present (base_q_idx 80/110/168) and this is the
   only block in the stream with `bsize == sb_size`, libaom's own special case
   (`decodemv.c`: `if (bsize != cm->seq_params->sb_size || !mbmi->skip_txfm)`), incl. the
   `delta_lf` sub-read.
2. `skip` / `is_inter` ctx at a 64x64 block.
3. `y_mode[size_group_lookup[BLOCK_64X64]=3]` CDF content (adaptation drift; frames 0-3
   never crossed a decision boundary).
4. `tx_depth`: `--enable-tx-size-search=0`, so TX_64X64 must code no depth symbol.
5. The 64x64 luma coefficient read (eob_pt alphabet at TX_64X64, `all_zero` ctx).

## Instruments already in the tree
* `EC_AV1_TRACE=1` -> `TRACE partition_w64 ... pre_rng=<range before the symbol>`.
* `EC_IIS=1` -> `TRACE iis px= py= side= w= h= sg= mode= skip=` at the square
  intra-in-inter `y_mode` read (decode.rs, right after `cdfs.y_mode[size_group_wh(..)]`).
* `/home/tahinli/.cache/sqdrift/h.c` -- libaom inverse-transform harness. Build:
  `gcc -O1 -o h h.c -I$SRC -I$BUILD -I$SRC/apps $BUILD/libaom.a -lm -lpthread`.
  MUST call `av1_rtcd(); aom_dsp_rtcd();` first or `av1_idct64` is a NULL fn ptr (segfault).
* The oracle has NO range print inside `read_intra_block_mode_info` /
  `read_delta_q_params`. Add one via `scripts/instrument-aom-oracle.sh` into a PRIVATE
  build dir; never rebuild the shared `~/.cache/aom-oracle/build/aomdec` in place.
