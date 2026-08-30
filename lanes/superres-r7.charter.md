# lane-superres r7 — is that edge filtered at all?

Read `lanes/superres-r6.report.md`. r5's decode work is verified green (237
passed, the target gate the only failure).

## Where the search stands
r6 localized the 2-pixel residue precisely: horizontal-edge deblocking at the
CODED frame's rightmost mi column, x0 = 44..47. It re-derived `filter14` /
`filter8` in Python against the bit-exact pre-deblock dump and reproduced our
Rust output exactly — **our filter arithmetic is spec-correct**, so the bug is
in edge/level SELECTION, not in the filter. It then instrumented two libaom
candidates (`set_lpf_parameters`, `set_one_param_for_line_luma` /
`set_lpf_parameters_for_line_luma`) and confirmed neither fires, and ran out of
budget hunting a third dispatch path.

## Before hunting libaom's dispatch further, test this hypothesis
The coded frame width here is 43; the mi grid is 48 wide. **x = 44..47 is the
mi-alignment margin, beyond `frame_width`.** Two things follow, and either would
produce exactly this shape:
1. We may be deblocking an edge libaom never filters, because it clips the loop
   to the coded width. "Neither candidate fires" is consistent with libaom
   simply not filtering there — an absent call is evidence, not a dead end
   (class `equal-range-means-unread`, applied to pixels).
2. We may be FEEDING the upscaler decoded margin pixels where libaom feeds
   replicated ones: `av1_superres_upscale` calls `av1_extend_frame_borders`
   first, which replicates from column `y_crop_width - 1`. r3's margin threading
   moved the count 5 -> 2, which looks like progress but is equally consistent
   with trading one wrong source for another.

Both are cheap to test and neither needs libaom's dispatch: clip our deblock to
the coded width and see; then feed the upscaler a replicate of column 42 instead
of decoded margin and see. Run them as experiments, not as a rewrite — one at a
time, reverting between, and report both results even if neither closes it.

If both fail, then go after the dispatch path with gdb as r6 planned, using its
reproduction: `EC_SUPERRES_STREAM_DUMP=<path>.obu cargo test -p ec-av1 --lib
stream::tests::a_real_aomenc_superres_key_frame_sequence_decodes_pixel_exact --
--nocapture`, then that `.obu` through `~/.cache/aom-oracle/build/aomdec` with
`EC_AV1_PREFILT_WIDE_DUMP` / `EC_AV1_POSTDEBLOCK_DUMP`, diffing frame 2 columns
44-47.

## Housekeeping this round owes
r6 left throwaway `EC_LF_TRACE` diagnostics in
`~/.cache/aom-oracle/src/av1/common/av1_loopfilter.c` — outside this repo and
uncommitted. Either fold them into `scripts/instrument-aom-oracle.sh` as a
proper env-gated rung (the shape every other rung uses, and then they survive an
oracle rebuild) or revert them. Do not leave edits sitting in the oracle's source
tree where the next lane will not know they are there.

## Then
Stage 4: inter-frame superres — scaled-reference MC, spec 7.11.3.3, libaom
`av1_setup_scale_factors_for_frame` / `av1_convolve_2d_scale`.

Merge note: main is at 92d8beb and has moved a long way (multi-tile decode
gated, CDEF index, chroma modes, a `bit_depth != 8` refusal, three reworded
partition refusals, and three guard tests that pin the never-exercised aomenc
tools, every decode-path refusal string, and the gates that swallow a decode
error). Consider `git merge main` here and resolving it yourself. Report every
refusal string you add, rename or remove, verbatim.

Hard rules: `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-superres`; foreground
`nice -n 19 cargo ... -j4`; every `cargo test` a timeout >= 600000 ms. Sibling
worktrees have live agents — never build in or edit them. Never push, never
merge into `main`. 75-turn cap, does not reset: COMMIT AT EVERY GREEN STEP.
End with `lanes/superres-r7.report.md`, VERDICT on line 1.
