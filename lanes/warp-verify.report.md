# lane-warp verification report

PASS (verification split: cross-provider seats attempted — zai timed out twice at 420-480s,
kimi capped at 60 calls after independently re-running `cargo test -p ec-av1` (217/0 PASS,
its words) — remaining source-level checks completed by the orchestrator directly against
the libaom C tree at /tmp/libaom-src; the C source is the foreign oracle for every claim).

## Claim 1 — interp-filter read suppression: VERIFIED, one bounded caveat
libaom reconinter.h:420 `av1_is_interp_needed`: exactly three suppressors —
`skip_mode` (:422), `motion_mode == WARPED_CAUSAL` (:423),
`is_nontrans_global_motion` (:424; blockd.h:1574 — GLOBALMV/GLOBAL_GLOBALMV only, min
block dim >= 2 mi, and every ref's global wmtype != TRANSLATION).
Ours: single-ref path passes `is_globalmv || warped_selected`, compound path
`is_globalmv || skip_mode` (compound can be neither warped nor... warped — motion_mode is
single-ref-only; skip_mode blocks are compound-only — the split covers the full matrix).
CAVEAT (bounded): our `is_globalmv` suppresses unconditionally; libaom's global arm also
requires min-dim >= 8px and non-TRANSLATION global wmtype. Our decoder codes global motion
as IDENTITY only (non-TRANSLATION per the wmtype check) and never reads switchable filters
below 8x8 (decode_inter_block8 leaves record [3,3]) — under those two standing constraints
the behaviors are identical. If TRANSLATION global motion is ever ported, this call site
must grow the wmtype check. Noted in-code at the eligibility comment block.

## Claim 2 — mvstack entry clamping: VERIFIED
libaom mvref_common.h:52 `clamp_mv_ref` bounds = mb_to_edge ± (GET_MV_SUBPEL(bw|bh) +
MV_BORDER), MV_BORDER = 16<<3 = 128. Ours (mvstack.rs clamp_mv_ref): to_edge ± (bw8|bh8 +
128) — computed equal (worked example, B01 of pinned warp-flake-5: both give 384; libaom
EC_MV pred=(0,384) matches ours post-fix). Both builders now clamp every entry
(candidates.iter_mut()), matching av1_find_mv_refs's end-of-build clamp loop; unit test
`a_candidate_far_outside_the_frame_is_clamped_to_it` asserts the entry contract.

## Claim 3 — extended-partition arms: VERIFIED BEHAVIORALLY
The HORZ_B/VERT_A/VERT_B arms' neighbour/ctx/record threading is proven by outcome:
three pinned aomenc streams decode 24/24 byte-exact through these arms (warp-mismatch,
warp-flake-5, warp-flake-7), the flipped warp gate ran 10/10 clean sweeps
(40 seeds each, EC_AV1_GATE_DUMP armed, zero mismatch dumps), and the full workspace
gate is 148/148. A desynced or mis-stamped arm cannot survive byte-exact pixel
comparison across 24-frame CDF-adaptive decode.
Residual (pre-existing, in r5 report): these arms are exercised only by streams where
partitions are pinned to 32 (gate recipe) plus desync-free content that uses them;
a dedicated rect/ab-partition-enabled gate remains deferred.

## Evidence index
- pins: `pinned_warp_stream_decodes_pixel_exact` (3 streams) green
- gate: 10x `a_real_aomenc_stream_with_warped_motion_refuses_or_matches` rc=0
- suite: ec-av1 lib 217/0; workspace 148/148 RC=0 (warp-fullgate2.log)
