# lane-refs — the reference set (LAST2) and the frame interpolation filter

Worktree `edith_codecs-refs`, branch `lane-refs` off main 68e96c56. Every gate
arm is the prebuilt release lib-test binary running `bd_rate_screen_native`
(12 pictures, `gop 12`, four quantizers, native `gate_crop` window); every row
is read off that log's own header line, never off the argument order.

## 0. The controls reproduce

| clip | control (vs libaom / vs rav1e) | charter |
|---|---|---|
| film A, 12 frames | +21.7 / -4.4 | +21.7 / -4.4 |
| film B, 12 frames | +26.9 / -0.6 | +26.9 / -0.6 |
| screen capture, 12 frames | +20.1 / -30.4 | +20.1 / -30.4 |

All three land to the digit.

## 1. LAST2 — the census, before the machinery

`encoder.rs`'s `ref_frame_idx` maps LAST2/LAST3 onto LAST's own DPB slot, so
the picture before LAST is not merely unoffered to the search, it is not
retained at all. Building it out is not a small diff (a second retained slot
per level, `ref_frame_idx` / `refresh_frame_flags` / `ref_frame_sign_bias` /
order hints for it, a per-reference mv stack, the writer's `single_ref` tree,
`record_mi`, and — the expensive part — a SECOND motion search per block, on
the stage that already owns most of the encode wall: `search_inter_block`'s
existing extra references are priced search-FREE for exactly that reason).
So the lever was PRICED first (`last2_census`, `--ignored`, 3.3 s):

every 16x16 luma block of the gate's own 12-picture window of both real films
gets one full-pel diamond search against the NEAR reference and the same
search against the FAR one, at the two lags the pyramid offers — (1, 2) for
the leaf chain, (4, 8) for the ARF level, which is rav1e's own pair.

| clip | level (near/far lag) | blocks | far wins | far wins >10% | SAD near | SAD best-of | prediction energy removed |
|---|---|---|---|---|---|---|---|
| film A | leaf (1/2) | 57600 | 38.7% | 19.0% | 20974762 | 19509622 | **6.99%** |
| film A | ARF (4/8) | 23040 | 37.8% | 21.0% | 12015492 | 10776319 | **10.31%** |
| film B | leaf (1/2) | 76800 | 38.1% | 21.2% | 18946199 | 17572121 | **7.25%** |
| film B | ARF (4/8) | 30720 | 44.3% | 22.5% | 8539904 | 7824079 | **8.38%** |

READ IT AS: a second past reference is chosen by ~4 blocks in 10 and by more
than 10% of SAD by ~1 in 5; best-of-two removes 7-10% of the total residual
ENERGY before any rate is paid for naming the reference or for the second
search. That is the largest un-taken prediction lever measured on this content
so far (the anchors' non-skip area is the standing film B gap), and the ARF
level — where the census is strongest — is exactly the level libaom and rav1e
spend their extra slots on.

**Decision: the lever is worth building, and it does not fit this lane.** The
census is the charter's own first step ("before wiring the full search"), and
the build is chartered as its own lane below.

## 2. The frame interpolation filter — WIRED, MEASURED, REJECTED as a default

The premise held: `mc::predict` hardwired `InterpFilterKind::Regular`, so the
encoder could not have coded a SMOOTH or SHARP frame whatever the header said.
The kernel now travels on `FrameCtx::interp_filter` (copied into every
tile-search worker by `filter_ctx_copy`, so a threaded search reads the same
kernel), and `encode_inter_frame` writes that one kernel into the header —
one place, so header and prediction cannot disagree. `EC_AV1_INTERP=
smooth|sharp` (or `set_frame_interp`) selects it; the default is unchanged
REGULAR and byte-identical.

| arm | film A | film B | keep rule |
|---|---|---|---|
| REGULAR (control) | +21.7 / -4.4 | +26.9 / -0.6 | — |
| SMOOTH | **+40.9 / +10.2** | +26.3 / -0.4 | film A loses 19.2 / 14.6 |
| SHARP | +24.0 / -2.9 | +28.4 / +0.6 | both rows worse on both columns |

SMOOTH is a catastrophe on film A (19 BD points) for 0.6 of a point on film B's
libaom column while its rav1e column goes 0.2 the wrong way; SHARP is worse
everywhere. The keep rule (both films down on both columns) is nowhere near
met, so **REGULAR stays the default at every preset** and the per-frame
CHOOSER was not built: its ceiling is film B's +0.6/-0.2 mixed-sign move, and
its downside when a proxy mispicks is film A's 19 points (ladder rung 1 — it
does not need to exist). The wiring stays because it is what makes the
measurement possible at all and because it removes the hardwired-kernel
ceiling; it is proved by a witness rather than left as a claim.

## 3. Invariants

* `each_frame_interpolation_filter_codes_its_own_stream_ffmpeg_decodes_exactly`
  (new, NOT ignored): each of the three kernels codes a DISTINCT stream (a
  kernel that never reached the prediction would code REGULAR's bytes — class
  `symbol consumption gap`) and ffmpeg reconstructs every frame of each of them
  exactly against our own reconstruction. Green.
* `cargo test -p ec-av1 --release --lib`: see §4.
* `cargo check --workspace --all-targets`: see §4.

## 4. Suite and check

`timeout 900 cargo check --workspace --all-targets -j4`: **0 errors, 0 ec-av1
warnings** (the 22 warnings are ec-opus' missing struct-field docs and
ec-vorbis' unused `decode_capture`, both pre-existing on main).

THE LIB SUITE DOES NOT FIT A 900 s CAP ON THIS BOX. Two whole-suite arms died
`RC=124` -- the second one alone on the prebuilt binary, 168 of 253
`stream::` tests in, no failure anywhere -- so the suite was SPLIT into three
disjoint-by-filter arms, each under the cap:

| arm | filter | result |
|---|---|---|
| A | `--skip stream::` | 333 passed, 0 failed, 30 ignored (661 s) |
| B | `stream:: --skip 10bit` | 197 passed, 0 failed, 15 ignored (822 s) |
| C | `10bit` | 42 passed, 0 failed, 1 ignored (99 s) |

Arms A and C overlap on exactly 2 tests (the two `10bit` names outside
`stream::`, counted from `--list`), and the binary holds 616 tests, so the
UNION is **570 passed / 46 ignored / 0 failed**, which accounts for every test
in the binary (570 + 46 = 616) and includes this lane's two: the filter
witness (passed) and `last2_census` (ignored).

Invariants, all green, all on the prebuilt release binary:

| invariant | run | result |
|---|---|---|
| every preset decodes sample-exact through both decoders | `--include-ignored`, `EC_COMP_MISMATCH=1` | 1 passed |
| tile / filter-stage bytes do not depend on the thread count | `--include-ignored` | 2 passed |
| facade identity + `predicted_coeff_bits_track_the_tile_the_writer_wrote` | default | 2 passed |
| pins 8562 / 33357 | `the_encoders_own_streams_are_byte_identical_to_their_pins`, in arm A | passed, UNCHANGED -- every lever ships off, so nothing was re-pinned |
| the filter witness | `each_frame_interpolation_filter_codes_its_own_stream_ffmpeg_decodes_exactly` | passed (landed in f6a107ff) |

## 5. Deferred

* **LAST2 / a real second past reference — deferred, chartered.** What it
  needs: DPB slot 7 is free (the key's copy, never read back), so the leaf
  chain can alternate its refresh between slots 0 and 7 and name the older of
  the two as LAST2 while the ARF levels take theirs from the anchor pair;
  `ref_frame_idx[1]`, `refresh_frame_flags`, `ref_frame_sign_bias`, the order
  hints and a per-reference mv stack follow; the cheapest first arm offers
  LAST2 through the EXISTING search-free `extra` path (NEARESTMV/GLOBALMV
  only, as GOLDEN is offered today) so the second motion search — and its wall,
  which is the user's stated priority — is a second arm, not a prerequisite.
  Witness: a clip where the picture before LAST is the better reference, N
  blocks coded off LAST2, exact through ffmpeg and `decode_stream`, hidden
  frame display order intact. Unblocked by: a lane of its own with the wall
  budget for the desync hunt.
* **The per-frame interpolation-filter chooser — dropped, not deferred**: §2's
  table is its refutation, not a gap.
* The long-GOP gate was not run: nothing from this lane changes a default, so
  there is nothing for it to confirm.
