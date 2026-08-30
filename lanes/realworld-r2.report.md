# lane-realworld r2 report

VERDICT: Step 1 (suite verify) and step 2 (CDEF gate) landed and committed
(080acfd on branch lane-realworld). Step 3 (delta_q/delta_lf) is
UNSTARTED -- next agent picks it up fresh, nothing half-done to unwind.

## Step 1 -- suite verify
`EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib -j4` on bd61617:
**232 passed, 0 failed, 17 ignored** -- matches the baseline exactly, no
fix needed. bd61617's CDEF code compiled clean and the suite was
already green; r1's risk (apply_cdef's guard) turned out fine in
practice (see below).

## Step 2 -- the CDEF gate (committed 080acfd)
`a_real_aomenc_stream_with_cdef_decodes_pixel_exact` in
`crates/ec-av1/src/stream.rs` (~4668-4870). Latest run:
**40/40 pixel-exact matches, 0 named refusals, `cdef_idx_hits()` 24-40
per run (hard-asserted > 0)**.

### The real finding: the charter's recipe (copy masked-compound's
64x64 single-SB fixture) is structurally incapable of firing cdef_idx.
`cdef.bits` selects among per-*superblock* strength profiles; a 64x64
frame is exactly one superblock, so aomenc's RD has nothing to
differentiate and always writes `bits=0` -- confirmed empirically
(20-attempt runs at 64x64, `cdef_idx_hits()` stayed 0 every time).
Needed a multi-SB fixture (128x64, 2 SBs) instead.

### Two pre-existing gaps this multi-SB fixture immediately hit (NOT
this lane's to fix, named so the next lane can find them by grep):
1. At `--cpu-used=0` (every other real-stream gate's setting), aomenc's
   RD on multi-SB content picks SB-level (part64) partition types this
   decoder's intra key-frame path doesn't cover -- only
   `PARTITION_NONE`/`PARTITION_SPLIT` are handled at
   `decode_key_frame_tile_with_cdfs`'s top match (`decode.rs` ~4684),
   everything else (`HORZ_4`=8, `VERT_B`=7 observed live) falls into the
   catchall `unsupported("a partition type this encoder never writes")`
   at `decode.rs:5049`. This reproduced with `--enable-rect-partitions=0
   --enable-ab-partitions=0 --enable-1to4-partitions=0` all set --
   aomenc chose them anyway (this build/cpu-used=0 combo doesn't fully
   honor those flags for the top-of-tree decision, empirically). The
   INTER path (`decode_inter_frame_tile_with_cdfs`, `decode.rs:9397`)
   has a worse latent bug for the same reason: it reads `part64` and
   then does `let _ = part64;` -- discards it and blindly recurses as
   if it were always `SPLIT`. If a real inter frame's SB ever writes
   anything but SPLIT there, this **silently desyncs** rather than
   refusing by name. Not exercised by this gate (avoided via
   `--cpu-used=4`, see below) but is a live landmine for any future
   multi-SB real-stream gate.
2. `--cpu-used>=1`'s default `aq-mode` writes `delta_q_present`/
   `delta_lf_present`, which this decoder already refuses by name
   (correctly) -- but that's exactly this lane's *next* step, so
   sidestepped for now with `--aq-mode=0 --deltaq-mode=0`.

### The working recipe: 128x64 (2 SBs), `--cpu-used=4` (steers RD away
from the unsupported part64 types), `--aq-mode=0 --deltaq-mode=0`
(keeps delta_q/delta_lf off), `--enable-cdef=1`, everything else same
shape as the masked-compound gate (rich inter toolset: warped-motion,
obmc, masked-comp, interintra-comp all =1, since a starved toolset was
also observed to push RD toward the same unsupported partition types
even at `--cpu-used=0`/min-max-partition=64).

No refusal strings were added or altered -- the pre-existing
"a partition type this encoder never writes" and "delta_q_present or
delta_lf_present" refusals are both genuine (proven by direct
reproduction, not assumed).

## Step 3 -- delta_q / delta_lf: NOT STARTED
Ran out of budget before beginning. Per charter: spec 5.11.15
`read_delta_qindex` / 5.11.16 `read_delta_lflevel`; libaom
`av1/decoder/decodemv.c` same names. Needs: new adapting CDFs (struct
field + defaults array + verify counter-reset coverage per
`cdf-counter-not-reset` class -- save/restore and reset2/reset3 are
already length-generic per `cdf_state.rs:590+`), running
quantizer/loop-filter-delta state threaded through block decode, read
once per SB at the first non-skip block. Its own gate/counter/commit.
The existing refusal ("a frame with delta_q_present or delta_lf_present
set") is reproducible on demand with `--aq-mode=1` or default
`cpu-used>=1` at 128x64+ -- a ready-made fixture recipe for that gate's
own "still refuses correctly until landed" check, then flip once wired.

## Remaining refusal strings verbatim (still true, not touched)
- `"a partition type this encoder never writes"` (SB-level, intra;
  `decode.rs:5049`) -- pre-existing, real, out of scope.
- `"a partition type this encoder never writes (value={part32})"` (32
  level, intra; `decode.rs:5042`) -- pre-existing.
- `"an INTER 32x32 partition type this encoder never writes
  (value={part32})"` (`decode.rs:10555`) -- pre-existing.
- `"a frame with delta_q_present or delta_lf_present set (this decoder
  never reads the per-superblock delta symbols)"` -- this lane's step 3
  target, still refuses correctly.
- `"a partition below 16x16 other than a clean split..."`,
  `"a partition below 8x8..."`, `"a 16x16 block whose true edge cuts
  through both axes..."` -- untouched, unrelated.

## Next lever
delta_q/delta_lf (charter step 3), exactly as scoped. The part64
multi-SB gap (finding #1 above) is a separate, larger lane -- flagged,
not fixed, not this lane's job.
