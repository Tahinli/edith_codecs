# lane-dgolomb — libaom film-B decode (the "Golomb tail" refusal)

## Root cause (closed)
`crates/ec-av1/src/motion_field.rs` `lower_mv_precision` implemented only the
`is_integer == false` half of libaom's function, on a comment claiming
`force_integer_mv` is unreachable because `allow_screen_content_tools` is
"refused outright at the stream level". Both halves of that claim are false:

* libaom's `av1_set_screen_content_options` counts 16x16 blocks with 2..=4
  distinct luma values, so DARK, SMOOTH FILM reads as screen content — every
  frame of the film-B gate clip has `allow_screen_content_tools = 1`;
* `av1_is_integer_mv` (encoder_utils.c:1357) returns 1 exactly when
  `cs_rate >= 0.8` **and** `C == T`, i.e. when the source frame is
  pixel-identical to the previously coded source. Film B has such frames.

So a real libaom stream carries `force_integer_mv = 1` on an inter frame, and
every temporal MV candidate must be rounded to full pel. Ours stayed at
eighth-pel → the mv stack held values libaom never produced → whole-frame
prediction mismatch behind a clean parse, and the entropy desync surfaced
blocks later as `a Golomb tail longer than this decoder reads`
(class `refusal-names-a-correlate`).

**Class: `branch-dropped-as-unreachable`** — a spec branch deleted on an
unreachability note that a later capability change silently invalidated.

## First divergence (measured, not reasoned)
Film-B gate clip (1920x1024 crop, `-cpu-used 6 -crf 35 -g 12`), decode-order
frame 3 = order_hint 1, the ONE inter frame with `force_integer_mv = 1`,
block mi(0,0) (128x128, NEWMV, ref LAST):

* `EC_TPL` field cells identical in both decoders (16 probes), range identical
  (`rng=49952`) — so the field and the entropy state were right;
* instrumented aomdec stack: `(16,0)w16 (16,-8)w12 (8,-40)w4`
* ours before the fix:   `(14,-4)w16 (12,-2)w8 (12,-38)w4 (16,-4)w4`
* `(74,-20)/5 → 15,-4 → integer → (16,0)` reproduces libaom exactly.

## Fix
`lower_mv_precision(mv, allow_high_precision_mv, force_integer_mv)` with
libaom's `integer_mv_precision` (ties away from zero on `|mod| > 4`), threaded
through `project_tpl_mv`/`add_tpl_ref_mv`, `TplArgs`/`CompoundTplArgs` and the
six `decode.rs` construction sites.

## Sweep (class + siblings)
* `grep` for unreachability claims over `ec-av1`/`ec-av1-syntax`: no other
  "spec branch dropped because a header flag is refused" site.
* The other decoder-side `is_integer` consumers in libaom —
  `av1_find_best_ref_mvs` (mvref_common.c:847) and the compound nearest/near
  lowering (decodemv.c:1519) — are provably no-ops for us once the temporal
  path is fixed: under `force_integer_mv` every other candidate source is
  already integer (`read_mv_component` honours the flag, `warp.rs:128` honours
  it for global MVs, spatial candidates are decoded MVs). Not implemented; if
  a future defect lands here, this is the place.

## Verification
* `crates/ec-av1/src/motion_field.rs` `force_integer_mv_rounds_a_projected_candidate_to_full_pel`
  — the five (mfmv0, rfo) pairs of that first block, eighth-pel vs full-pel,
  values taken from the oracle. Fails without the fix. PASSES.
* Film-B ladder, 48 frames, `-cpu-used 6 -g 48`, our `decode_stream` vs
  `ffmpeg -c:v libdav1d`, sample-exact over all 48 shown frames:
  crf 20 EXACT, crf 35 EXACT (was the Golomb refusal), crf 45 EXACT.
  Same at `-g 12`. Before the fix: crf 20/45 25–28 dB garbage, crf 35 refused.
* `crates/ec-av1/src/stream.rs` `a_libaom_force_integer_mv_stream_decodes_exact`
  — synthetic 1920x1024 source (no film sample in `fixtures`): near-flat dark
  background (2..=4 luma values per 16x16 block, the detector's own rule) with
  a textured patch stepping 9 px every SECOND frame, so half the frames are
  exact duplicates (the `C == T` trigger). Asserts (a) libaom really coded an
  inter frame with `force_integer_mv` + `use_ref_frame_mvs` (anti-blindness),
  (b) all 12 shown frames decode sample-exact vs ffmpeg. PASSES, 2.1 s.

## OPEN / deferred
1. **The synthetic gate is BLIND to this defect.** Re-run with the integer
   branch stubbed out (`if false {`) and it still passes: on that source the
   temporal candidates are already full-pel (or unused), so only the unit test
   above actually regresses the fix. Fixing this needs a synthetic clip whose
   duplicate frame still reads NON-multiple-of-8 projected temporal MVs —
   e.g. motion of an odd number of half-pels between the earlier frames, with
   the duplicate pair placed so the ARF's field survives into it. Unblocks:
   one more recipe iteration against `EC_TPL`/`EC_STACK`.
2. **crf 5 still fails**: `-cpu-used 6 -crf 5 -g 48` desyncs on the KEY frame
   (base_q 9, `delta_q_present`, `cdef_bits 3`), first divergent intra block
   mi(184,344) — aomdec `EC_IMODE ... rng=49148` vs ours `rng=52010`, the
   previous traced block being mi(188,340). Unrelated to temporal MVs; needs
   its own bisect from that block's coefficient ladder (`EC_TRACE_COEFF`).
3. **Not done in this lane** (budget): the "reference streams decode exact
   through our decoder" assertion inside `external_ladder`/the native gates;
   the full `cargo test -p ec-av1 --release` run (started detached, was still
   running at hand-off, log
   `/tmp/claude-1000/.../scratchpad/suite.log`); `cargo check --workspace
   --all-targets`; the byte pins (8590 / 28535) were not re-read.
