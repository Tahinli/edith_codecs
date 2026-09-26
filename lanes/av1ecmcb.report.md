# lane-av1ecmcb report -- the EC_MCB rung's halved window

Branch `lane-av1-ecmcb`, base `fe3e8418`. Single item: close the deferred
`EC_MCB`-rung coordinate defect named in `lanes/av1llpred.report.md` ("still
windows its own dump with `write_w / 2` -- fix-now(deferred to whoever next
touches that rung)").

## Verdict: defect REAL, fixed (instrumentation only)

`decode.rs::obmc_run`'s rung filtered the `EC_MCB="<plane>:<px>:<py>[:<frame>]"`
window and dumped the pre/post-blend chroma rows with `write_w / 2` /
`write_h / 2`. `write_w`/`write_h` are the block's LUMA write dims
(`decode_inter_block` ~30346: `cpx = px >> ss_x`), so the `/2` is correct only
at 4:2:0; at 4:4:4 (`ss 0/0`, decoded by this tree -- refusal inventory names
only 4:2:2) the chroma block is full-size and the old rung quartered both its
window and its row dumps.

Oracle semantics (r13/r14 ground truth): aomdec's private `EC_MCB` printed
every `dec_build_inter_predictors` call whose PLANE-pixel span covers the
pixel. The saved pairing dumps still pin this:
`~/.cache/t900-tmp/aom_mcb41b.txt` prints `bw=16 bh=16` + 16x16 rows for the
32x32-luma block at mi(176,192) -- plane pixels -- and `our_mcb41.txt` matched
1:1 at 4:2:0. Plane pixels at `ss 0/0` are luma-sized, so the fix is
`write_w >> ss_x(fctx)` / `write_h >> ss_y(fctx)` in the window check and both
row dumps (bit-identical at 4:2:0: dims are even, `>>1 == /2`).

Note: the oracle's private `EC_MCB` patch is no longer in
`~/.cache/aom-oracle/src` (t900-era tree reverted to pristine); the saved r13
dumps are the surviving oracle ground truth and are what this closure pairs
against.

## Verification

* 4:2:0 pairing unchanged: `EC_MCB=1:353:395:41` on the pinned
  `~/.cache/t900-tmp/c900_62.obu` (release `decode_probe`), block
  `cpx=352 cpy=384 cw=16 ch=16` -- all 16 prerows byte-equal to the saved
  oracle rows AND to the pre-fix `our_mcb41.txt` rows.
* 4:4:4 fail-before / pass-after: `aomenc --profile=1 --enable-obmc=1` gate
  recipe (obmc gate's exact flags, `gradients` seed 43, 64x64, 25 frames,
  `/tmp/ecmcb/s43.obu`, 444). `EC_MCB=1:48:48`: pre-fix binary fires 0 blocks
  (48 is outside `cpy..cpy+16` for the `cpx=32 cpy=32` block); post-fix fires
  `OUR_MCB pre f=5 cpx=32 cpy=32 cw=32 ch=32` with full 32-wide rows.
* Decode behavior unchanged: the diff touches only `env_flag!("EC_MCB")`-gated
  lines (the closure is `.then`-gated, so with the env unset nothing new
  runs). Gate `a_real_aomenc_stream_with_obmc_decodes_pixel_exact`:
  `ok. 1 passed` (pixel-exact vs ffmpeg), genuinely ran (no SKIP).
* `cargo check -p ec-av1` (`CARGO_TARGET_DIR=$HOME/.cache/cargo-target-av1ecmcb`):
  0 warnings.

## EVIDENCE

EVIDENCE: ~/.cache/t900-tmp/aom_mcb41b.txt + /tmp/ecmcb/our_now_block.txt | EC_MCB=1:353:395:41 both decoders on c900_62.obu, block cpx=352 cpy=384 | 16/16 prerow values equal to the oracle's saved plane-pixel rows (post-fix)
EVIDENCE: /tmp/ecmcb/s43.obu + /tmp/ecmcb/post48.txt | aomenc --profile=1 --enable-obmc=1 (obmc-gate recipe, seed 43) + EC_MCB=1:48:48 | pre-fix 0 fires, post-fix `cw=32 ch=32` full-block dump at 4:4:4
EVIDENCE: cargo test -p ec-av1 --lib a_real_aomenc_stream_with_obmc_decodes_pixel_exact | ok. 1 passed; 0 failed (decode path untouched)

## Disposition

fix-now (the deferred item): the rung is ss-aware and pairs at both chroma
shapes. No decode-behavior change shipped. The left-pass `OUR_MCB left` tmp
dump still prints the BLEND's own `cbw/cbh` (which reflect the blend's
per-axis halves) -- that is the rung instrumenting real decode geometry, not a
rung-side assumption, and stays as-is.
