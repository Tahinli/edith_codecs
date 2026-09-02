# lane-sbrect10 r1 handoff (turn cap)

Branch `lane-sbrect10` (worktree `/home/tahinli/Documents/Code/Rust/edith_codecs-sbrect10`),
based on lane-r14 a2e2e29 with `main` (7a47fc1) merged in cleanly (merge commit in history).

## STATUS: root cause FOUND and FIXED; sibling gate GREEN both depths; the new dedicated gate
## still needs an 8-bit fixture (its 8-bit arm currently FAILS "gate proved nothing").

## Pinned stream
`$HOME/.cache/sbrect10/a14.obu`, sha256 (hashed twice, identical)
`e85d3b84180de05cd7af769daa7b4f0662389fe0cbf5f0dc8119e4a036b6cf44`.
Regenerate: `bash $HOME/.cache/sbrect10/gen.sh <out>` (192x128, 6 frames, cq 61, 10-bit,
horizontal split geq, motion step 12 = the sibling gate's 10-bit attempt 14).

## First mismatch (before the fix)
decode-order frame 2 (display == decode order, `--lag-in-frames=0`), plane Y, 15973/24576 luma
samples + 874 V; frames 0 and 1 exact. Per-64x64-SB wrong-pixel map row0 = [128, 332, 3931],
row1 = [3912, 3728, 3942]; SB(0,0)'s 128 are rows 62-63 only (deblock bleed from below).

## Range ladder (EC_TRACE_MODE / EC_TRACE_MODE_STEP / EC_TRACE_COEFF / EC_AV1_TELL vs
## instrumented aomdec `~/.cache/aom-oracle/build/aomdec`)
- Partition symbols: 40/40 identical (values + positions) -> no rect strips anywhere, every
  inter SB is PARTITION_NONE 64x64. Confirms the sibling gate's "zero rect strips" note.
- Our `TELL label=post_interp_filter range=` equals aomdec's `EC_MODE_VAL ... rng=` for every
  inter block up to the divergence (frame 2 block mi(0,16): both 61730). Mode info in sync.
- First diverging element: frame 2, block mi_row=0 mi_col=32 (SB(0,2), x=128..191), which BOTH
  decoders read as INTRA inside an inter frame (`is_inter=false`, our TELL post_is_inter
  range=42400). aomdec then reads its luma TU with entry rng=38192 and all_zero=1; ours reached
  all_zero=0 at rng=63221 with eob=56 -> desync inside that block's intra mode syntax.
- Root cause: `uv_mode` was read off the 14-symbol CFL-allowed CDF (`cdfs.uv_mode_cfl`) on a
  64x64 block. libaom `is_cfl_allowed` (blockd.h, spec 5.11.5) = `block_size_wide <= 32 &&
  block_size_high <= 32`, so the real alphabet is the 13-symbol `uv_mode_no_cfl`. Same DC_PRED
  VALUE, wrong interval narrowing -> silent desync (class `wrong-alphabet-same-value`, second
  instance after lane-sbpart r3).

## Fix (committed)
- `crates/ec-av1/src/decode.rs` intra-in-inter block path: `cfl_allowed = write_w.max(write_h)
  <= 32`, picks `uv_mode_no_cfl`, and gates the `cfl_alphas` read.
- Same rule enforced ONCE at the head of `read_intra_mode_rect` and `read_intra_mode`
  (`let cfl = cfl && bw.max(bh) <= 32` / `side <= 32`), so no caller can offer CFL above 32.
- New counter `NOCFL_UV_MODE_HITS` / `decode::nocfl_uv_mode_hits()` fires exactly when the
  size rule removed CFL.
- Result on the pinned stream: all 6 frames byte-identical to `ffmpeg -pix_fmt yuv420p10le`
  (was [0,0,16847,23688,19028,17491] wrong samples).

## Gates
- `a_real_aomenc_inter_sequence_with_a_superblock_level_rect_partition_decodes_pixel_exact`
  flipped to `[8u32, 10u32]` and is GREEN: 8-bit 2 refusals / 4 pixel-exact carrying the arm /
  64x32=4 / 32x64=4 / 64-axis TUs=2 / 10 out-of-scope with 0 mismatches; 10-bit 2 refusals /
  2 carrying / 64x32=2 / 32x64=2 / 64-axis TUs=2 / 12 out-of-scope with 0 mismatches.
- NEW dedicated gate `a_real_aomenc_inter_frame_with_a_64x64_intra_block_reads_the_no_cfl_uv_alphabet`
  (stream.rs, just above the lane-inter4 32x32 rect gate): 16-attempt grid, both depths, every
  decode-order frame compared, asserts `nocfl_uv_mode_hits` moved. 10-bit arm passes; the
  **8-bit arm FAILS** ("no attempt decoded an intra block above 32x32") -- with the smooth
  translating source aomenc never picks a >32 intra block in an inter frame at 8 bit
  (probed cq 45/52/58/61/63, with and without `noise=all_seed=7:alls=30`: 0 hits every time).

## Exact next step (measured, ready to paste)
Replace the gate's source with the half-random fixture, which DOES produce the shape at 8 bit:
`geq=lum='if(lt(X,128), 40+mod(floor((Y+N*12)/32)*90,200), mod((X*7+Y*13+N*97)*31,256))'`
Probe results (`$HOME/.cache/sbrect10/probe.sh <geq> <cq> <depth> <extra>`, counts >32
intra-in-inter blocks and pixel diffs vs ffmpeg):
  8-bit  cq52: 4 hits but DECODE-ERR (named refusal) | cq58: 2 hits, 0 pixel diffs |
         cq61: 3 hits, 0 pixel diffs
  10-bit cq52: 6 hits, 0 diffs | cq58: 7 hits, **32890 diffs** | cq61: 7 hits, **39907 diffs**
So: use cq 58/61 for the 8-bit arm, cq 52 for the 10-bit arm, OR (better) treat the 10-bit
cq58/cq61 half-random mismatch as the NEXT defect of this lane -- it is a real, reproducible
10-bit failure on a stream this decoder does not refuse, and it is NOT the CFL bug (that one is
fixed). Triage it with the same ladder (EC_AV1_TELL post_interp_filter vs aomdec EC_MODE_VAL,
then EC_TRACE_COEFF entry ranges).
Then: full suite via
`systemd-run --user --unit=sbrect10-suite-$(date +%s) -p MemoryMax=10G --same-dir bash -lc 'EC_NOMEMGUARD=1 EC_AV1_REQUIRE_AOMENC=1 CARGO_TARGET_DIR=$HOME/.cache/cargo-target-sbrect10 nice -n 10 cargo test -p ec-av1 --lib -j3 > $HOME/.cache/sbrect10-suite-r1.log 2>&1'`
(NOT run yet this round), then `lanes/sbrect10-r1.report.md`.

## Ruled out
- Rect partitions / 64-axis residual: zero of both in the failing stream (partition trace 40/40
  identical), so the sibling gate's "another shape" note was correct.
- MV stack, motion_mode alphabet, interp filter, interintra: all exonerated by the
  post_motion_mode / post_interp_filter range equality with aomdec on every block up to the
  diverging one.
- Encoder-recipe doubt: stream hashes identical across two generations.
