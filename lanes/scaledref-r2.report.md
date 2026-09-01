# lane-scaledref r2 — warp-capable frame under a scaled reference read the wrong motion_mode alphabet

Branch `lane-scaledref` (HEAD back on the branch: the verifier left the worktree detached at
a8111dd; `git checkout lane-scaledref` restored it, both were the same commit, no work lost).

## Verifier finding 1 — ROOT CAUSE FOUND AND FIXED (no refusal needed)

The verifier's premise was that "motion_mode symbol reading is unaffected; only the prediction
falls back". That is **false**, and it is the whole defect. libaom
`motion_mode_allowed` (oracle source `av1/common/blockd.h:1484-1487`):

```c
    if (mbmi->num_proj_ref >= 1 && allow_warped_motion &&
        !xd->cur_frame_force_integer_mv &&
        !av1_is_scaled(xd->block_ref_scale_factors[0])) {
      return WARPED_CAUSAL;
    }
    return OBMC_CAUSAL;
```

Under a scaled reference the block reads the **2-symbol `obmc_cdf`**, never the 3-symbol
`motion_mode_cdf`. We read the 3-symbol one, which narrows the arithmetic decoder by the wrong
amount: the decoded *value* happened to stay SIMPLE_TRANSLATION, so nothing ever errored — the
stream decoded "OK: 24 frames" with quietly wrong pixels, exactly the silent-wrong-pixels class
the verifier caught. The warp guard at decode.rs ~10197 could never see it: `warp_params` is
`None` for those blocks (`warped=0` in EC_AV1_TRACE), so no fallback and no refusal ran.

| path:line | why |
| --- | --- |
| `crates/ec-av1/src/decode.rs` ~10060 (`warp_eligible`) | `&& !ref_is_scaled` added (`mc::scale_factor(py_ref.width, frame_width) != REF_NO_SCALE`), libaom-exact; new `SCALED_WARP_SUPPRESSED_HITS` counts the blocks that take the `obmc_cdf` arm because of it |
| `crates/ec-av1/src/decode.rs` ~630 | the counter + `scaled_warp_suppressed_hits()` / `mixed_scale_compound_hits()` accessors |
| `crates/ec-av1/src/decode.rs` ~9515 | `MIXED_SCALE_COMPOUND_HITS`: compound blocks with exactly one scaled tap (diagnostic for finding 3) |
| `crates/ec-av1/src/decode.rs` ~11455 (`decode_inter_block8`) | sibling site of the same shape, comment only: an 8x8 leaf under a scaled reference is refused before this code runs, so the same `&& !ref_is_scaled` is owed by whoever lifts that refusal |
| `crates/ec-av1/src/stream.rs` (superres gate) | `--enable-warped-motion=1` is now the DEFAULT arm (`EC_SCALEDREF_WARP=0` only for bisecting), `--superres-mode` made settable (`EC_SCALEDREF_MODE`), and `scaled_warp_suppressed_hits() > 0` is hard-asserted so the warp arm cannot pass vacuously |
| `crates/ec-av1/examples/decode_probe.rs` | `EC_PROBE_OUT=<path>` dumps the decoded planes as raw yuv420p — that is the instrument that located this defect (diff vs `ffmpeg -f obu`) |
| `lanes/scaledref-r1.report.md` | finding 2, wording corrected (below) |

Not changed, reported instead of silently fixed: libaom also requires `!cur_frame_force_integer_mv`
in that same condition and our `warp_eligible` still omits it. Left alone this round — no gate
covers `force_integer_mv` (screen content), and changing an alphabet choice without a gate is the
mistake this round is fixing. deferred: force_integer_mv warp eligibility — unblocked by a
screen-content gate with `cur_frame_force_integer_mv=1`.

Refusals: **none lifted, none added**, 45 unchanged. `refusal_inventory` / `gate_coverage`
untouched and green (see totals). The "warp prediction with a scaled reference" refusal stays: it
now only guards a *global*-warp block under a scaled reference, which this recipe never produced
(`scaled_warp_fallback=0`).

EVIDENCE: /tmp/.../scratchpad/warp_superres.obu + ours2.yuv vs ff.yuv | EC_PROBE_OUT decode of the verifier's pinned stream, byte-compared to `ffmpeg -f obu -pix_fmt yuv420p -f rawvideo` | before: frames 1..23 differ (frame 1: 1342 luma + 370 chroma pixels, first at (2,26), max |d| 3, growing to 18 by frame 23); after the fix: `cmp` identical, 147456/147456 bytes

EVIDENCE: gate run, this report | `EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib a_real_aomenc_superres_stream_with_compound -- --nocapture` (warp now on by default) | `0 other-capability refusals, 12/12 pixel-exact, superres_hits=291 predict_scaled_hits=3198 scaled_compound=500 scaled_warp_fallback=0 scaled_obmc=9 scaled_interintra=1 scaled_block8=0 scaled_warp_suppressed=25 mixed_scale_compound=0` — 25 blocks took the `obmc_cdf` arm only because of this fix

## Verifier finding 2 — report correction (done)

`lanes/scaledref-r1.report.md` line ~27 now reads "every SHOWN frame compared to ffmpeg ... a
hidden alt-ref is checked only through propagation" (ledger already records
`decode_stream` pushes only under `show_frame`, stream.rs:711).

## Verifier finding 3 — mixed scaled/unscaled compound taps: DEFERRED, measured not assumed

Counter `mixed_scale_compound` added and wired into the gate's print; both alternative recipes were
run, neither yields a gateable stream:

- `EC_SCALEDREF_MODE=3` (qthresh): aomenc never turns superres on for this content/q —
  `superres_hits=0 predict_scaled_hits=0`, i.e. the whole gate goes vacuous (it fails its own
  `predict_scaled_hits > 0` assert, which is the assert working).
- `EC_SCALEDREF_MODE=2` (random denominator per frame): produces coded widths that are not
  multiples of 32, so the frame edge forces sub-16 partitions and the stream stops at the still-open
  named refusal `an 8x8 partition leaf under a scaled reference (superres, unimplemented)` at
  seed 42.

deferred: mixed one-scaled-one-unscaled compound tap — unblocked by the leaf8 desync (lane-sub8),
which is what mode-2 recipes hit; the counter is in place so any future recipe proves the case
immediately. The per-tap code itself is r1's (`scale0`/`scale1` are already independent).

## Test totals

`EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib` (whole crate lib suite): **268 passed, 0 failed, 23 ignored**, 968s (same totals as r1, now with the warp arm of the superres gate ON). Run at the final tree.

## Residue carried from r1 (unchanged)

- deferred: 8x8 partition leaf under a scaled reference — unblocked by the lane-sub8 leaf8 desync.
- deferred: reference scaling in BOTH axes (`--resize-mode`) — unblocked by a lane sized for it.
- accepted: 10-bit is refused upstream (`Picture` planes are `Vec<u8>`).
- accepted: the Golomb-tail cap stays where r1 left it (it masks an intra-rect64 defect).
