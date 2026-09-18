# Lane vp9pix: VP9 inter-frame PIXELS (motion compensation → reconstruction)

Branch `lane-vp9-pix`, base `main 597b7442`. Crate scope: `crates/ec-vp9` only.
Worktree `/home/tahinli/Documents/Code/Rust/edith_codecs-vp9pix`, private
`CARGO_TARGET_DIR=$HOME/.cache/cargo-target-vp9pix`.

Deliverable: `decode()` decodes inter frames through motion compensation,
residual reconstruction, the loop filter and reference-slot management, and is
byte-identical to a locally built libvpx 1.15 oracle on every fixture swept.

## Baseline (milestone 0)

    $ CARGO_TARGET_DIR=$HOME/.cache/cargo-target-vp9pix cargo test -p ec-vp9
    exit=0   (20 test binaries, 0 failures, 0 ignored)

No pre-existing reds, so no base triage was needed. (Pre-existing warnings:
`crates/ec-vp9/src/modes.rs` unused `partition_probs`/`x_mis`/`y_mis` +
`read_partition`, `tables/mod.rs` `TM_PRED` — untouched files, present at base.)

## The gate: byte-exactness against libvpx 1.15

Oracle: the instrumented build the syntax lane left at `/tmp/vp9loss`
(`drv_pix.c` is new here: it appends every SHOWN frame's cropped I420 planes,
frame-indexed, to `PIXDUMP`). Rich fixtures copied to `$HOME/.cache/vp9pix/`
for tmpfs insurance. Comparator: `$HOME/.cache/vp9pix/pixcmp.py`
(oracle, ours, w, h) → per-frame identical/first-diff report.

    $ python3 $HOME/.cache/vp9pix/pixcmp.py oracle_inter1.pix ours_inter1.pix 1920 1080
    oracle=9331200 bytes ours=9331200 bytes frame_bytes=3110400
    frame 0: IDENTICAL (3110400 bytes)
    frame 1: IDENTICAL (3110400 bytes)
    frame 2: IDENTICAL (3110400 bytes)

That is the acceptance gate: 3/3 shown frames of the 1080p single-tile inter
stream, loop filter ON, planes byte-for-byte.

Corroborating instruments (all agree):
- `SKIPLF` oracle + `EC_VP9_SKIP_LF=1` ours: identical too (isolates MC+residual
  from the filter).
- Per-superblock LUMA loop-filter masks: oracle `LFMASKSY=1` vs our
  `EC_VP9_LFMASKSY=1` → **0 of 510 SBs differ, on all three frames.**
- Emitted loop-filter edges: oracle `LFTRACE=<frame>` vs ours
  `LFTRACE=1 EC_VP9_LF_FRAME=<n>` (our per-frame gate) → every oracle edge is
  emitted by us (oracle-only = 0); our extra 2660/3116 lines are the *second
  column of a dual H16 pair* — libvpx prints one line for
  `vpx_lpf_horizontal_16_dual`, we print one per 8x8 cell. Verified 2660/2660
  and 3116/3116 of the extras have an oracle twin at `(x-8, y)`.

The entry-gate fixture is `/tmp/inter1.ivf` (1080p, 1 tile column). It is
reachable through `tests/scratch_pixdump.rs`
(`INTER_IVF=… EC_VP9_PIXDUMP=… cargo test -p ec-vp9 --test scratch_pixdump -- --nocapture`).

## Corpus sweep (real content)

| fixture | frames | result |
|---|---|---|
| `vp9-1080p-23.976-8bit.ivf` (48) | 48 | **all byte-identical** |
| `vp9-2160p-23.976-8bit.ivf` (48) | 48 | **all byte-identical** |
| `vp9-superframe-altref.ivf` (320x240, 60) | 60 | **all byte-identical** |
| `/tmp/inter1.ivf` (1080p, 1 tile col) | 3 | **all byte-identical** |
| `/tmp/inter.ivf` (1080p, 2 tile cols) | 3 | **all byte-identical** |
| `vp9-1080p-60-8bit.ivf` (120) | 72 | identical 0..71, **desync at 72** (deferred, below) |

Sweep driver: `$HOME/.cache/vp9pix/sweep.sh <fixture> <w> <h>` (oracle dump +
our dump + `pixcmp.py`).

## Milestones

1. **Pixel plumbing** — oracle `drv_pix.c` + `tests/scratch_pixdump.rs` +
   `pixcmp.py`. Done; the comparator is the instrument every claim below uses.
2. **Reference slots** — `refs: [Option<RefFrame>; 8]`, `RefFrame` holds the
   frame at its **64-aligned extent** (`Arc<Planes>`) plus its coded size.
   `refresh_frame_flags` is applied after the frame's loop filter
   (`swap_frame_buffers`), so a later frame predicts from filtered pixels.
   Evidence: the 1080p GOP and the 60-frame altref stream decode identically,
   which is only possible if each frame's prediction reads exactly the frame
   the header names (any slot mistake shows as a pixel diff on frame 1+).
   The refusal for a size-changing reference is named (below).
3. **Motion compensation** — new `crates/ec-vp9/src/mc.rs`: libvpx's
   `clamp_mv_to_umv_border_sb`, `build_mc_border` (per-block reference border
   extension — the VP9 decoder does NOT extend reference borders in place),
   `dec_build_inter_predictors` including the "window inside the frame" direct
   read, the four convolve kernels (`EIGHTTAP`, `EIGHTTAP_SMOOTH`,
   `EIGHTTAP_SHARP`, `BILINEAR`, index order = `vp9_filter_kernels`), compound
   averaging, and `average_split_mvs` (luma per sub-block, 4:2:0 chroma q4
   average).
4. **Residual** — tokens via the shared `decode_inter_tx_tokens` (one read per
   transform block), then `inverse_transform_add` with the DEFAULT scan and
   `DCT_DCT` (`inverse_transform_block_inter`).
5. **Loop filter** — per-cell level from `get_filter_level`
   (`lvl[seg][ref_frame[0]][mode_lf_lut[mode]]`), the skip promotion
   (`!less8x8 && eobtotal == 0 → skip`) and the skipped-inter mask rule
   (`skip && is_inter` keeps only the prediction masks). Verified by
   mask-identity + edge-set + pixels (above).
6. **Gates** — `cargo test -p ec-vp9` green (below); refusals re-checked;
   prose updated.

## Bug classes found while porting (all fixed, each with its evidence)

1. **Direct-read coordinates taken after the tap padding.**
   `dec_build_inter_predictors` computes `buf_ptr = ref_frame + y0*stride + x0`
   BEFORE `y0 -= INTERP_EXTEND - 1` / `x0 -= …`; our first version read the
   direct path from the padded coordinates, i.e. 3 rows/cols early. Signature:
   border-path blocks exact (top/bottom SB rows), direct-path blocks wrong by
   up to ±5, and `SKIPLF`-style isolation shows the error is in prediction.
   Independent python re-derivation of the C predictor from the oracle's
   reference frame identified it in one step (oracle 45 vs ours 47).
2. **Skipped intra blocks inside an inter frame skipped
   `dec_reset_skip_context`.** libvpx calls it at the top of `decode_block`
   (before the intra/inter split); ours only in the inter arm, leaving stale
   above-context bytes → a `tile bool decoder desync` in frame 2, with the
   first divergence a *prob* difference at the same ordinal read (the skill's
   "same ordinal, different question" signature).
3. **Skipped inter blocks kept full loop-filter masks.** `grids.skip_inter`
   was only set by the eob promotion, so a coded-skip inter block got the
   size/tx masks instead of prediction-only masks → extra 16-tap edges
   (mask-identity check caught it as `L16 ⊉ L32`).
4. **Intra-in-inter tx type.** libvpx's luma mode for a sub-8x8 block is the
   SUB-BLOCK's (`mi->bmi[…]`), used for `intra_mode_to_tx_type_lookup`; the
   merged syntax walk used `info.mode`. Now shared by the syntax and pixel
   walks (they must agree or the two paths read different tokens).
5. **`mv_ref_list[2]` write (crash).** `ADD_MV_REF_LIST_EB`'s successful
   second add is `goto Done`; in the "different reference frame" loop the C
   leaves the loop, our port kept iterating and wrote past the two-entry list
   (`inter.rs`, now an explicit `break`). Found by the corpus sweep —
   `vp9-superframe-altref.ivf` panicked at frame 14 before the fix; 60/60
   frames byte-identical after.
6. **Frame-level interpolation filter numbering** (`read_interp_filter`):
   the header's literal index runs through
   `literal_to_filter = {EIGHTTAP_SMOOTH, EIGHTTAP, EIGHTTAP_SHARP, BILINEAR}`
   — NOT libvpx's `#define` numbering (`EIGHTTAP 0`). Converted where
   `frame_interp` is built; inert on switchable streams (the fixture's), correct
   for non-switchable ones.

## Named refusals (unchanged or added)

- `vp9 profile N` / `vp9 subsampling` — unchanged (existing tests).
- `vp9 intra-only` — unchanged refusal, now also reachable from `decode()`.
- **`vp9 inter reference size change`** (new): a named reference whose coded
  size differs from the current frame is refused by name (libvpx scales it;
  scaling is not ported in this lane).
- **`vp9 inter odd frame dimensions`** (new): odd width/height refused by
  name — the keyframe crop stores chroma at `floor(w/2)` while the predictor
  reads libvpx's `uv_crop_width = (w+1)/2`; the two disagree only for odd
  extents, so those are refused rather than silently wrong.

## Deferred

- **`deferred(fix-now-next-lane)` — frame 72 desync on
  `vp9-1080p-60-8bit.ivf`.** Frames 0..71 are byte-identical; frame 72 is an
  ordinary inter frame (`MODEDUMP` CHSUM: `tx_mode=4 ref_mode=0 allow_hp=1
  interp=4 intra_only=0 er=0`, matching its neighbours) and dies with
  `tile bool decoder desync`. An unexercised inter syntax path (compound
  reference / sub-8x8 / a specific `use_prev_frame_mvs` state) is the likely
  home; the 1080p 24fps GOP, the 4K GOP and the altref stream do not reach it.
  Repro: `$HOME/.cache/vp9pix/sweep.sh vp9-1080p-60-8bit 1920 1080`.
- `deferred(unblock: odd-extent chroma layout)` — the odd-dimension refusal
  above; the root fix is to store reference chroma at `(w+1)/2`, which also
  changes the keyframe path's returned plane layout.
- **Superseded claim:** `lanes/vp9inter.report.md` ranked cause 3 ("keyframe
  multi-tile-column desync on `/tmp/inter.ivf` frame 0") does **not**
  reproduce at this lane's base — `INTER_IVF=/tmp/inter.ivf INTER_EXPECT=none
  cargo test -p ec-vp9 --test scratch_interdump` passes on the untouched main
  checkout (f715d70c, the tile-row fix, landed before the inter-syntax merge
  32d81e8f). `/tmp/inter.ivf` now decodes all 3 frames byte-exactly.

## Gates

    $ CARGO_TARGET_DIR=$HOME/.cache/cargo-target-vp9pix cargo test -p ec-vp9
    exit=0 — every binary green, including the new witness
      tests/inter_pixels_exact.rs  (2 tests: GOP + GOP with tile columns,
                                    ffmpeg libvpx rawvideo oracle, self-generated
                                    fixtures; non-vacuity proof: with
                                    EC_VP9_SKIP_LF=1 both tests FAIL at frame 0
                                    byte 31 — ours 81 vs ffmpeg 82)
      tests/keyframe_exact.rs      (4 tests, incl. the rewritten
                                    inter_stream_decodes_and_matches_ffmpeg:
                                    60/60 frames of the altref stream vs ffmpeg)

## Repro (all commands)

    # gate
    cd /home/tahinli/Documents/Code/Rust/edith_codecs-vp9pix
    CARGO_TARGET_DIR=$HOME/.cache/cargo-target-vp9pix cargo test -p ec-vp9

    # 1080p single-tile inter stream vs the instrumented oracle
    cd $HOME/.cache/vp9pix && PIXDUMP=$HOME/.cache/vp9pix/oracle_inter1.pix ./drv_pix inter1.ivf
    cd /home/tahinli/Documents/Code/Rust/edith_codecs-vp9pix
    CARGO_TARGET_DIR=$HOME/.cache/cargo-target-vp9pix INTER_IVF=$HOME/.cache/vp9pix/inter1.ivf \
      EC_VP9_PIXDUMP=$HOME/.cache/vp9pix/ours_inter1.pix \
      cargo test -p ec-vp9 --test scratch_pixdump -- --nocapture
    cd $HOME/.cache/vp9pix && python3 pixcmp.py oracle_inter1.pix ours_inter1.pix 1920 1080

    # mask identity / edge sets
    LFMASKSY=1 ./drv_pix inter1.ivf 2>&1 | grep '^LFY' > lfy_oracle.txt
    LFTRACE=1 ./drv_pix inter1.ivf 2>&1 | grep '^LFTR' > lf_oracle_1.txt   # LFTRACE=<frame>

## VERDICT

GATE 1 — `cargo test -p ec-vp9` green in the worktree (private target dir),
including the new pixel witnesses: **PASS**.
GATE 2 — `inter1.ivf` `decode()` returns 3 shown frames, each byte-identical to
the libvpx 1.15 oracle's planes (comparator output above): **PASS**.
GATE 3 — per-milestone evidence lines above; every pixel mismatch met during
development is named with its root cause (6 classes): **PASS**.
GATE 4 — this report: baseline, per-milestone evidence, `inter.ivf`
disposition, deferred items with dispositions, repro commands: **PASS**.
GATE 5 — committed to `lane-vp9-pix`, `git status` clean, no push/merge: see
the git log below.
GATE 6 — VERDICT section: this one.

FLAGS (the verifier should re-derive these independently)
1. **The direct-read coordinate rule** (class 1) is the single most load-bearing
   line in `mc.rs`: `origin` must come from the MV-adjusted, NOT tap-padded,
   `x0`/`y0`. Re-derive from `vp9_decodeframe.c:672-728` and check our
   `x0_mv`/`y0_mv` usage.
2. **`skip_inter` semantics**: `skip && is_inter` at BOTH the coded-skip and
   the `eobtotal == 0` promotion, and the mask early-return placement (after
   the above/left prediction masks). Mask-identity can be re-run per SB with
   `EC_VP9_LFMASKSY=1` vs `LFMASKSY=1`.
3. **The `mv_ref_list[2]` fix** changes an existing (merged) syntax path: the
   altref fixture panicked before it and is byte-exact after — re-run
   `vp9-superframe-altref.ivf` end to end.
4. **Reference storage extent**: refs must keep the 64-aligned plane buffer
   (the decoded overhang rows are readable by the direct path). Cropping them
   reproduces the class-1 symptom set; confirm by checking that frames 1+ of
   the 1080p GOP match (the overhang is read at the frame bottom).
5. **The 60fps frame-72 desync** is a real open defect: verify it is
   independent of this lane's changes (`scratch_interdump` on the base
   checkout reports the same frame-72 desync in the syntax walk).
6. Corpus sweeps were run on this workstation with `/tmp` fixtures; the
   1080p/2160p/altref dumps are regenerable from `sweep.sh`.

## MERGE-SIDE FOLLOW-UPS (2026-09-19)

- Merge: fast-forward `597b7442..7f1ff7d5` from the main checkout; the
  create-list carried exactly the four expected files (`src/mc.rs`,
  `tests/inter_pixels_exact.rs`, `tests/scratch_pixdump.rs`, this report) —
  no junk-file class.
- Independent verification before merge: reviewer re-derived all eight claim
  families (own oracle+Rust pixel dumps, corpus sweeps 48/48, 48/48, 60/60,
  frame-72 desync reproduced AND proven lane-independent at base 597b7442 via
  `scratch_interdump`, non-vacuity reproduced byte-for-byte, all six bug-class
  fixes audited against the libvpx sources). VERDICT: PASS, confidence 0.94.
  Residual-risk note: the MV clamp is applied unconditionally where libvpx
  gates it — benign on these streams, recorded for the next inter lane.
- Fixtures: the lane's permanent tests self-generate their streams with
  ffmpeg/libvpx at run time, and the worktree's `fixtures/` was a symlink to
  the main checkout's — no gitignored fixture copy was needed.
- Dependent-crate sweep: nothing depends on `ec-vp9` (the dependency edge is
  `ec-vp9 -> ec-vp9-syntax`, untouched by this lane).
- Merged-tree gates, run on main after the ff:
  `cargo test -p ec-vp9` — 20 test binaries, 0 failed, 0 ignored;
  `cargo check --workspace --all-targets` — clean, warning parity with the
  declared baseline (modes.rs `partition_probs`/`x_mis`/`y_mis`,
  `read_partition`, tables `TM_PRED`).
- `tests/scratch_pixdump.rs` SKIPs when `INTER_IVF`/`EC_VP9_PIXDUMP` are
  unset and panics naming the path when set-but-missing — workspace runs are
  safe after tmpfs reaps.
- Kept for the next lane (deliberately not deleted): the `EC_VP9_TRACE` /
  `EC_VP9_PIXDUMP` / `EC_VP9_SKIP_LF` / `EC_VP9_LFMASKSY` hooks, the scratch
  harnesses, and `$HOME/.cache/vp9pix/` (oracle `drv_pix`, fixtures,
  `pixcmp.py`, `sweep.sh`).
- Lane worktree `../edith_codecs-vp9pix` and its private target dir were
  removed after the merge; branch `lane-vp9-pix` stays.
