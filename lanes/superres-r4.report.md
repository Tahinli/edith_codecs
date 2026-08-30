VERDICT: WIP -- f167362 verified compiling and 237/238 green; target gate still 2/4096 pixels off, root cause narrowed but not closed

## What this round did
r3's work at f167362 was committed by the orchestrator at its 75-turn cap,
never seen to compile. Job one per the charter: verify it.

- `cargo check -p ec-av1 --lib --tests`: clean (warnings only, no errors).
- `EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib -j4`: **237 passed, 1
  failed, 17 ignored**. The only failure is the target gate,
  `a_real_aomenc_superres_key_frame_sequence_decodes_pixel_exact`. r3's two
  landed fixes (superres-kf-denominator flag, chroma-crop rounding, and this
  round's own margin-threading commit) cost nothing elsewhere -- no
  regressions against the 232/0 baseline this tree started from.
- No code changes landed this round (all diagnostic instrumentation added
  during investigation was reverted with `git checkout --` before finishing;
  `git status` is clean at f167362).

## The residue, narrowed
r3 closed 3 of 5 mismatched pixels with the wide-margin threading fix
(committed in f167362). The remaining 2 (frame 2, rows 17/18, output column
62 -- the `--superres-kf-denominator=12`, 43->64 gate) were traced further
this round:

1. Dumped pre-deblock and post-deblock pixel values at columns 40-47 for
   frame 2, rows 8-23 (temporary `eprintln`, reverted). The only edge that
   touches these rows is a single horizontal deblock edge at `y0=16`,
   `level=5`, category `len=14` (the wide-filter class), covering columns
   40-47 in two 4-column groups.
2. Hand-computed `filter8`'s flat-branch formula (`decode.rs`'s
   `filter14`/`filter8`) against the dumped pre-deblock values for every
   affected column (43, 44, 45) and every affected row (13-18) --
   **matched our own post-deblock output exactly in all cases tried**. The
   tap-weight arithmetic itself is self-consistent with the spec 7.14.6.3/
   7.14.6.4 formulas; no bug found there.
3. Confirmed the margin our decoder stashes for superres (`real_right` in
   `upscale_row`) is exactly this same post-deblock data -- so if the
   pre-deblock reconstruction and the deblock level/mask decision are both
   correct, the margin is correct too.
4. Ruled out a false lead: swapping row 17's and row 18's *margin* arrays in
   the pin harness (`scripts/superres-pin-harness.c`'s `row17-margin`/
   `row18-margin` cases) happens to reproduce ffmpeg's expected value for
   both rows. This looked like a smoking-gun row-index swap, but the two
   margins differ by at most 1 in any position (smooth gradient content) --
   entirely consistent with the *real* defect being a single-unit error
   that happens to land near a rounding boundary shared by both rows, not
   an actual row transposition. Src/margin pairing was independently
   confirmed correct at the Rust level (both come from the same row `r` of
   the same `y.data` buffer via the same `crop` closure).
5. Ruled out: CDEF (frame flags force `cdef.*_strength == 0`, `apply_cdef`
   returns immediately); intra-prediction edge availability using the wrong
   width (`frame_width` is never read inside `decode_key_frame_tile_with_cdfs`
   except at the crop/margin-stash tail -- prediction always sees the true
   mi-aligned width).

## What's not yet ruled out
- The deblock **level** itself (`edge_params`'s `cur_level != 0 ? cur_level
  : pv_level`, `decode.rs:3980`) for this specific margin-straddling edge --
  verified the *arithmetic* given `level=5`, not whether `5` is the level
  real libaom computes for this edge. This logic already passes broader
  deblock gates elsewhere (both intra and inter), so a narrow, edge-case-only
  divergence (e.g. a block whose transform straddles `frame_width` itself)
  is the remaining live suspect.
- The pre-deblock reconstruction at columns 44-47 (the block whose right
  half falls in the margin) was hand-verified for internal consistency, not
  against real libaom -- there is still no ground truth for the *pre-deblock*
  row content, only for the final post-superres pixel (which is 2 rounding
  steps downstream: deblock, then the upscale convolution).

## Next step (needs libaom instrumentation, not more hand tracing)
Per r3's own charter fallback: build a small aomdec `EC_TRACE`-style patch
(matching the one already used for msac/partition tracing elsewhere in this
lane family) that dumps the post-deblock, pre-superres row content at
`frame_width..true_width` for a real decode of this fixture, and diff it
column-by-column against `decode.rs`'s stashed margin. That is the only
instrument that can distinguish "our level/mask decision is wrong" from
"our level/mask decision is right and the pre-deblock reconstruction is
wrong" -- hand-tracing the arithmetic (this round) cannot, since it can only
prove self-consistency, not correctness against libaom's real output.

## Not reached
Stage 4 (inter-frame superres, spec 7.11.3.3) -- blocked on stage 3 closing
first, per both r3's and this charter's own ordering. Untouched this round.

## Commands
- `cargo check -p ec-av1 --lib --tests` -- clean.
- `EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib -j4` -- 237 passed, 1
  failed (the target gate), 17 ignored, 193s.
- `EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib
  a_real_aomenc_superres_key_frame_sequence -j4 -- --nocapture` -- FAILED,
  same 2-pixel mismatch (frame 2, rows 17/18, col 62 of the upscaled output).

## Refusal strings
None added, renamed or removed this round.
