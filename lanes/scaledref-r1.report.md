# lane-scaledref r1 — scaled-reference (superres) MC beyond single-ref, plus the Golomb tail

Branch `lane-scaledref`, off main `3808cf8`. Round 1 continues the previous
builder's uncommitted state (preserved as `95b3e70`) — that snapshot is
verified, gated and kept; everything below r1 adds is on top of it.

## What changed

| path:line | why |
| --- | --- |
| `crates/ec-av1/src/mc.rs:474` `horizontal_scaled_pass` | spec 7.11.3.3's horizontal walk factored out of `predict_scaled` and given to `predict_compound_intermediate` (new `x_scale_fp` param), so each compound tap scales off its OWN reference's luma width |
| `crates/ec-av1/src/decode.rs` (compound branch, ~9470) | the "compound-reference block with a scaled reference" refusal replaced by two independent `mc::scale_factor` values threaded into all 6 compound MC calls (Y/U/V x 2 taps) |
| `crates/ec-av1/src/decode.rs` `obmc_neighbour_pred` / `obmc_blend` | each OBMC neighbour is re-predicted through the scaled walk against **its own** reference's scale (`frame_width` threaded in) |
| `crates/ec-av1/src/decode.rs` (~10170) | warp under a scaled reference: libaom `allow_warp` (reconinter.c:41) suppresses local AND global warp and predicts translationally; that suppression is implemented, but the case is still refused by name (see residue) |
| `crates/ec-av1/src/decode.rs` `decode_inter_block8` | single-ref/compound/OBMC MC of the 8x8 leaf threaded for a scaled reference (`frame_width` param) |
| `crates/ec-av1/src/decode.rs` `read_golomb` (~1349) | **r1**: lift implemented, then REVERTED (`abec872`) -- it exposed a hidden defect, see below. Only a comment recording the finding + repro remains |
| `crates/ec-av1/src/decode.rs` `obmc_mask` + `obmc_blend` | **r1**: `obmc_mask_2 = {45,64}` added and one-sided chroma OBMC for an 8x8 luma block (`av1_skip_u4x4_pred_in_obmc`, reconinter.c:820, `DISABLE_CHROMA_U8X8_OBMC == 0` ⇒ chroma skipped in the ABOVE pass, kept in the LEFT pass). Before this, the first real 8x8 OBMC block hit `unreachable!` in `obmc_mask` |
| `crates/ec-av1/src/stream.rs` | the gate below |
| `crates/ec-av1/src/refusal_inventory.rs` | 2 refusal strings gone (compound-scaled; OBMC/interintra-scaled folded into a warp-only string); 47 → 45 |

## Gates

1. `stream::tests::a_real_aomenc_superres_stream_with_compound_obmc_and_interintra_decodes_pixel_exact`
   — real `aomenc --superres-mode=1 --superres-denominator=16 --superres-kf-denominator=16
   --auto-alt-ref=1 --lag-in-frames=25 --enable-obmc=1 --enable-interintra-comp=1
   --enable-smooth-interintra=1`, 12 seeds x 24 frames of 64x64 moving gradients, every frame
   (hidden alt-refs included, `decode_stream` decodes the whole OBU sequence) compared to ffmpeg.
   Hard-asserts `mc::predict_scaled_hits() > 0` and the per-case counters
   `scaled_compound/scaled_obmc/scaled_interintra > 0`.

   `EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib a_real_aomenc_superres_stream_with_compound -- --nocapture`

   EVIDENCE: /tmp/.../scratchpad/gate1.log | aomenc superres d=16 + compound/OBMC/interintra, 12 seeds x 24 frames decoded and compared to ffmpeg | 12/12 pixel-exact, 0 refusals, superres_hits=291 predict_scaled_hits=3198 scaled_compound=500 scaled_obmc=9 scaled_interintra=1

2. (withdrawn) `decode::tests::golomb_tails_read_back_past_the_old_twenty_bit_cap` — written, passing,
   and reverted with the change it gated. See "The Golomb tail" below.

## Refusals lifted (2)

- "a compound-reference block with a scaled reference (superres, unimplemented)" — gate 1, 500 hits.
- "warp/OBMC/interintra prediction with a scaled reference (superres, unimplemented)" — narrowed to
  "warp prediction with a scaled reference"; OBMC (9 hits) and interintra (1 hit) proven by gate 1.

`refusal_inventory` and `gate_coverage` tests green.

## The Golomb tail — a refusal that is masking a real defect (finding, fix-now for its owner)

Implemented as charted (`ee1f980`): read up to 32 leading zeros (dav1d's `len < 32`) instead of
erroring at 20, u64 accumulator, plus a round-trip gate over every prefix length 0..=31 through
`tile::write_golomb`. Bit-identical for every tail an encoder writes — our own writer tops out at 19
leading zeros (`tile::MAX_LEVEL == MAX_BR_LEVEL + (1 << 19)`), and libaom errors long before.

It still turned a green gate RED:
`a_real_aomenc_stream_with_a_superblock_level_horz_vert_partition_decodes_pixel_exact` failed at
**seed 67, frame 0 luma**. Bisected against main `3808cf8` and against the lane's own WIP `95b3e70`
(both pass), and main's run prints, for that very seed:
`seed 67 refusal: unsupported: AV1 tile (a Golomb tail longer than this decoder reads)`.

So the cap is not a capability limit — it is the point where an intra rect64 stream that has ALREADY
desynced first asks for something impossible (a key-frame coefficient level above `1 << 19`).
Class `refusal-hides-a-defect`. Reverted in `abec872`; `d8788fb` records the finding, the repro and
"lift this together with that defect's fix" at the cap itself.

EVIDENCE: cargo test output, this report | ran the rect64 gate at 3808cf8 / 95b3e70 / lane HEAD with and without the lift | pass / pass / FAIL seed 67 frame 0 luma; main prints the Golomb refusal for seed 67

fix-now for the intra rect64 owner: encode that gate's seed-67 fixture, raise the cap locally, and
bisect the first mismatching block — the long tail is a downstream symptom, not the defect.

## Test scope actually run

- `cargo test -p ec-av1 --lib a_real_aomenc_superres_stream_with_compound` → 1 passed, 0 failed.
- `cargo test -p ec-av1 --lib a_real_aomenc_stream_with_a_superblock_level_horz_vert_partition_decodes`
  → 1 passed, 0 failed (this is the gate the Golomb lift turned red; green again after the revert).
- `cargo test -p ec-av1 --lib refusal_inventory` → 3 passed; `... gate_coverage` → 2 passed.
- `cargo test -p ec-av1 --lib golomb` → 3 passed (at ee1f980, before the revert).
- A whole `cargo test -p ec-av1 --lib` run was started twice: the first completed and showed exactly
  one failure (the seed-67 one above, since fixed by the revert); the second was killed mid-run by
  the environment, so no full-suite total is claimed here. The verifier owns the full suite.

## Residue

- fix-now → **not** done, deferred: **warp under a scaled reference** stays refused. The libaom
  `allow_warp` fallback is implemented (warp suppressed, translational prediction used) but the
  previous builder's `--enable-warped-motion=1 --superres-denominator=16` seed-47 stream still
  mismatched ffmpeg at frame 2 luma. Shipping the lift would be wrong pixels behind a green name.
  deferred: warp+superres — unblocked by bisecting that stream (set `EC_SCALEDREF_WARP=1` on gate 1
  to reproduce; `EC_AV1_GATE_DUMP=<path>` pins the mismatching stream).
- deferred: **8x8 partition leaf under a scaled reference** — the leaf's MC *is* threaded for
  scaling, but no aomenc recipe produces a firing, decodable fixture. r1 measured this instead of
  assuming it: with `--min-partition-size=8` and a 64x**72** fixture (bottom 16-row band straddles
  the true edge, which is the only way this decoder reaches `decode_inter_block8`), aomenc does emit
  8x8 leaves — and the decode **desyncs inside the leaf** (`from_switchable_symbol` handed a 4th
  symbol, mc.rs:200), which is the same below-8x8 desync lane-sub8 r2 left open and has nothing to do
  with scaling. Refusal restored with that fact in the comment; deleting those six lines is the whole
  lift once the leaf8 desync is fixed. — unblocked by the lane-sub8 leaf8 desync.
  That experiment did produce one real fix, kept: `obmc_mask(2)` + one-sided chroma OBMC (above).
- deferred: **"a reference picture whose height does not match this frame's own true size"** — not
  attempted. Superres never scales height, so a height-mismatched reference can only come from
  `--resize-mode` (reference scaling in BOTH axes). That needs a vertical counterpart to
  `horizontal_scaled_pass` (per-row `y_step_qn`, `im_h` intermediate rows, libaom
  `convolve_2d_scale`) *and* a gate whose frame size changes mid-stream. — unblocked by a lane sized
  for resize-mode.
- accepted: **10-bit not covered.** `Picture` planes are `Vec<u8>`; 10-bit input is refused before
  any of this code runs (see memory `his-av1-library-is-10-bit`, lane-hbd10). The 8/10-bit half of
  the charter's gate line is unreachable from this lane.
- Reported conflict, not silently resolved (charter premise): the charter states the spec's `read_golomb` caps at 32
  leading zeros. Locally verifiable sources disagree — libaom `read_golomb` (decodetxb.c:30) calls a
  21st prefix bit a CORRUPT FRAME, which is exactly what the old refusal mirrored; dav1d reads up to
  32. There is no AV1 spec copy on this box and the network is unreachable from the sandbox
  (`curl https://aomediacodec.github.io/av1-spec/av1-spec.html` timed out), so the change follows the
  permissive decoder (dav1d) and is bit-identical to the old reader for every tail any encoder writes.
- Off the film path: neither of the user's two films uses superres (lane-hbd10), so nothing here moves
  their decode. The Golomb and OBMC-mask fixes are generic.
