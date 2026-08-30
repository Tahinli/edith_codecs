VERDICT: NOT FIXED -- localized (not root-caused) within budget. The refusal stays exactly as r4 left it, no code changed on HEAD.

## Pin recaptured (r4's pin was lost to a scratchpad; re-derived this round)

`gm.model == Affine` refusal path is untouched. Temporarily disabled *only* the "more than
one concurrently active ROTZOOM/AFFINE ref slot" refusal (`if false && ...` in
`stream.rs`, reverted before commit -- `git diff --stat` against HEAD is empty right now)
and re-ran `a_real_aomenc_stream_with_interintra_wedge_decodes_pixel_exact` with
`EC_WEDGE_GATE_ATTEMPTS=40 EC_AV1_REQUIRE_AOMENC=1 EC_AV1_GATE_DUMP=/tmp/claude-1000/gm-r5-pin.obu`.
Seed 43 mismatched again, dumped to `/tmp/claude-1000/gm-r5-pin.obu` (session scratchpad --
not committed; will need recapture next round the same way, `n_attempts=40` reliably reaches
seed 43's stream).

**Determinism confirmed** (ladder step 1): `scratch_isolate_pinned_mismatch` with
`EC_AV1_PIN=/tmp/claude-1000/gm-r5-pin.obu EC_AV1_PIN_N=24`, run twice back-to-back --
byte-identical result both times:
- frame 13: all 3 planes MATCH.
- **frame 14 luma: 441 mismatches (worst delta 7), first at offset 18 (row 0, col 18),
  ours=157 theirs=158.** u/v also mismatch (86/109 pixels, delta up to 4).
- frame 15: 576 mismatches, same first-offset (row 0, col 18), worst delta grows to 14 --
  the error compounds forward through prediction, consistent with r4's finding, not a fresh
  independent bug.

Same frame index (14), same first-offset (row 0, col 18) as r4's lost pin -- this is the same
defect shape, re-confirmed on fresh bytes.

## Localization (ladder step 2, done; step 3/4 NOT completed)

Frame 14's header: `global_motion = [Identity, Identity, Identity, Rotzoom(GOLDEN, params
[132096,137216,61480,56,-56,61480]), Rotzoom(BWDREF, params
[258048,268288,57452,-34,34,57452]), Identity, Identity]`, `reference_select = false`,
`primary_ref_frame = 3`, `allow_high_precision_mv = false` -- matches r4's shape exactly
(GOLDEN + BWDREF concurrently ROTZOOM).

Traced (temporary `EC_GM_DEBUG`/`EC_GM_DEBUG2` prints in `decode.rs`, reverted): across the
*whole* 24-frame stream only **7 blocks total** ever take the new global-warp branch
(`warp_params.is_none() && is_global_mv_block && ...`) -- 4 in frame 1 (the r4-fixed,
pin-verified single-ref-LAST case) and 3 in frame 14/15 (`ref_frame=5`/BWDREF only: mi
`(0,8)`, `(8,0)`, `(8,8)`, all bw4=bh4=8). **`mi=(0,0)` -- the 32x32 leaf that owns the
mismatched pixel at row 0, col 18 -- never appears in that trace at all.**

`EC_AV1_TRACE` on that exact block (frame 14, `mi_row=0 mi_col=0`):
```
skip=0 is_inter=1 mv=(-6,-4) is_new_mv=true bsize=32 ref=4 filter=[0,1]
motion_mode_eligible=0 obmc_selected=0 warped=0
```
i.e. plain **NEWMV** against **GOLDEN_FRAME** (ref=4), `is_globalmv=false` (confirmed via
`EC_GM_DEBUG2`: `is_global_mv_block=false` for this block despite `gm_model=Rotzoom` on
GOLDEN's slot -- the block chose NEWMV/NEARMV over GLOBALMV, so GOLDEN's own active ROTZOOM
model is never read by this block at all). No OBMC (`motion_mode_eligible=0` -- no
overlappable neighbours yet, it's the top-left leaf), no warp, no interintra, no compound
(`reference_select=false`). Ruled out as amplifiers within budget: CDEF (`y_pri_strength`/
`y_sec_strength` all-zero -- CDEF is a no-op this frame), loop restoration (`uses_lr: false`),
and deblocking reach (col 18 is 14px from the nearest coding-block boundary at col 32, well
outside an 8-bit loop filter's ~3px reach) -- none of the frame's own filter settings can
carry a warp-block error from `mi=(0,8)` sideways into `mi=(0,0)`'s interior at col 18.
Hidden/no-show alt-ref frames also ruled out: the pin's OBU stream has exactly 24 `Frame` OBUs
for 24 output frames -- no hidden-frame class defect here (checked per the ledger's
"gate-blind-to-hidden-frames" class before spending more time on it).

**What this means:** the mismatched pixel is NOT produced by the new global-warp code path at
all -- it comes from an ordinary NEWMV/GOLDEN translational block whose own motion vector
(predictor + coded delta) or subpel interpolation is apparently computed fine in isolation
(this exact machinery -- `mc::predict_with_filters`, mv-stack NEWMV predictor construction --
is exercised pixel-exact by dozens of other passing gates with no active global motion at
all). The only property distinguishing this frame from every match-so-far frame is the
concurrently-active GOLDEN+BWDREF ROTZOOM pair in the header -- so per the charter's own
framing, the defect is most likely a per-frame value that the mv-stack/NEWMV-predictor
construction reads from `global_motion`/`GmMvTable` and that should vary per candidate ref but
doesn't when two slots are simultaneously non-identity (e.g. the GLOBALMV stack-fill
candidates `mvstack.rs` appends per ref, or a `gm_table` lookup keyed wrong when scanning
neighbours across two active refs) -- but I did NOT find the exact site. `mvstack.rs`'s own
per-ref accessors (`gm_mv(gm, ref_frame)`, `is_global_mv0`/`is_global_mv1` branches at every
site grepped) all looked correctly keyed on inspection; nothing jumped out as shared state.

**Ladder step 3 (msac RANGE comparison) NOT run**: `~/.cache/aom-oracle/build/aomdec`'s
`EC_TRACE` instrumentation (`decodeframe.c:1233`) only covers `read_partition`
(`EC_PART`/`EC_PART_VAL`), not the mv-stack/NEWMV-predictor symbols this hypothesis needs --
extending it would mean patching+rebuilding `decodemv.c` in the oracle, which did not fit this
round's remaining budget. So prediction-vs-entropy is still open: a wrong NEWMV predictor
(prediction-side, no bits misread) and a wrong `drl_mode`/`mv_joint` symbol read from a
corrupted context (entropy-side) both fit the observed symptom (small, spatially local,
propagating-forward error, not catastrophic desync) equally well from what I proved this
round.

## Refusal count

Unchanged: still the two named refusals from r4 (`Affine`, `>1 concurrently active
ROTZOOM/AFFINE ref slot`). No code change landed, so the wedge gate's refusal mix is whatever
r4 already reported (30/40 named refusals, 10/40 matches, `wii_hits=4` in that round's run;
this round's own wedge-gate invocation was run with the multi-slot refusal temporarily
disabled specifically to capture the pin, not as a measurement of the gate's normal mix).

## Files touched

None on HEAD -- `git diff --stat` against `lane-gm2`'s base is empty. All edits this round
(the `if false &&` disable of the refusal, `EC_GM_DEBUG`/`EC_GM_DEBUG2` eprintln probes in
`decode.rs`, a `global_motion`/`reference_select` eprintln in `stream.rs`'s
`scratch_isolate_pinned_mismatch`) were reverted before finishing so as not to ship an
unverified decode or dead debug code.

## Next round

Two ways to close this without guessing:
1. Patch the aomdec oracle's `decodemv.c` to `EC_TRACE`-dump `read_drl_idx`/NEWMV predictor
   symbols + `r->ec.rng`, range-ladder frame 14's `mi=(0,0)` NEWMV block specifically, and
   settle prediction-vs-entropy for real (ladder step 3, this round's actual gap).
2. Or, cheaper: hand-compute the expected mv-stack for that exact block (donor neighbours: at
   `mi=(0,0)` in the top-left corner of a single-SB 64x64 frame there ARE none -- it's the
   very first block decoded, so its only stack contributions are the frame-level
   `GLOBAL_GLOBALMV`/zero-mv fallback + `find_mv_stack`'s own extra-search additions) against
   spec 7.10.2's `add_extra_mv_candidate`/`setup_ref_mv_list`, since a first-block-in-frame
   case has almost no neighbour state to account for -- narrowing the search space
   considerably versus a mid-frame block.
Re-pin with the same recipe (`EC_WEDGE_GATE_ATTEMPTS=40`, `EC_AV1_GATE_DUMP`) if the
scratchpad pin at `/tmp/claude-1000/gm-r5-pin.obu` doesn't survive to next session --
recapture reliably reproduces seed 43 / frame 14 / row0-col18.

Claude-Session: https://claude.ai/code/session_01T6cfkyThENXszWWQqYpuC4
