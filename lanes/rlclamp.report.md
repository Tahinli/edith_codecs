# lane-rlclamp — the pyramid rate loop at its quantizer floor

Worktree `edith_codecs-rlclamp`, branch `lane-rlclamp` off main `8d9d056b`.
Two fix commits, head `6da9cd06`. Class: **[[vorbis-rate-loop-windup]]** — a
flat/clamped spend the loop cannot correct — with a second instance of
*the re-plan feeds the level that cannot spend* found and fixed inside it.

## 0. The defect, reproduced

`encoder::tests::bitrate_target_lands_within_5_percent_over_48_frames`, base
`8d9d056b`, census on (`EC_AV1_RL_CENSUS=1`, this lane's new instrument) and
the redistribution ablated (`EC_AV1_RL_NOREDIST=1` = the old code):
**2 Mbit/s with the pyramid lands −4.8%**, and the 12 hidden frames code the
same bytes at 1536 kbit/s and at 2 Mbit/s (22348 B/frame in both) — the anchor
is at its floor and the extra 464 kbit/s has nowhere to go.

## 1. Census — `EC_AV1_RL_CENSUS=1`

One stderr row per frame the loop steers: frame index, level, slot, that
level's byte target, coded bytes, the slot's `base q`, the quantizer the frame
WANTED (`raw_q` = base + level offset), the one it got (`eff_q`, after the
coder's 1..=255 clamp), whether the clamp bound, and the run's remainder.

```
RLCENSUS n=2  level=Arf  slot=1 target=13291 actual=25961 base_q=12.0 raw_q=-8  eff_q=1 bound=true  left_bytes=485435 left_w=111.3
RLCENSUS n=3  level=Leaf slot=2 target=4246  actual=1265  base_q=21.4 raw_q=112 eff_q=112 bound=false left_bytes=459474 left_w=108.2
```

Where the clamp binds, per arm (48 pictures, 640x384, gop 48):

| arm | frames | bound frames | which |
|---|---|---|---|
| 384 kbit/s pyramid | 43 steered | 0 | — |
| 768 kbit/s pyramid | 48 | 0 | — |
| 1536 kbit/s pyramid | 48 | 11 | every hidden frame after the first |
| 2 Mbit/s pyramid | 48 | 11 | top ARF `raw_q` −20..−31, mid ARF `raw_q` −8, both coded at `eff_q` 1 |
| every flat arm | 48 | 0 | the flat path has no offset, so nothing clamps |

**Where the shortfall went (before).** The re-plan divided the run's whole
remainder by the whole remaining WEIGHT, and the hidden levels carry 88 of the
run's 153 weight units — so most of each frame's unspent bytes were handed
straight back to the level that had just failed to spend them. The leaf target
sat at 4246 → 4230 → 4376 → 4630 B for the first 33 frames of the 2 Mbit/s run
(leaves coding 3.7-5.1 kB against it) and only ran away at the end —
15049, 17135, 20335, 24537 B over the last five frames, where the leaf slot's
own `q` had reached 0 and it coded 12.7 kB frames it could no longer be paid
for. 24 kB of target at frame 46 is the shortfall arriving too late to spend.

## 2. The fix

`RateLoop` now carries one `LevelPlan` per LEVEL of the run (key, each distinct
hidden offset, leaves): its share of the run's weight, how much of that is
still to code, whether its last frame was at the coder's bound, and the
leaf-equivalent bytes it measured there. `replan` holds every bound level at
its MEASURED rate, takes its remaining weight out of the division, and
re-solves the scale over the levels that can still move — libaom's
`rc_pick_q_and_bounds` carrying `vbr_bits_off_target` against per-frame bounds,
in this encoder's own level model. The carry is bounded at BOTH ends
(`REPLAN_FLOOR` 0.25x, `REPLAN_CLAMP` 4x a level's opening share) and every key
frame resets each level's remaining weight to its whole share, so no debt
outlives a run.

**Per level and not per slot — the first cut was wrong and its number is
here.** Slot 1 mixes the top ARF (offset −32) and the mid ARF (−8): at the
floor both code ~24 kB, at weights 4x apart, so the mid's leaf-equivalent rate
projected onto the top's remaining weight drove the remainder NEGATIVE, every
target to zero, and `update` returns on a non-positive target — the loop stopped
dead after three frames and the 1536 kbit/s pyramid arm landed **−20.7%**
(`bitrate-after.log`, commit `92fde72a`). The per-level plan plus the floor is
what commit `6da9cd06` ships; the floor alone makes a zero target impossible.

## 3. Unit test — red then green

`encoder::tests::a_bound_level_hands_its_unspent_bytes_to_the_others`: a
synthetic per-level model. The key codes its plan exactly, then ONE hidden
frame is coded at `raw_q` 0 spending half its plan, and the leaves' target must
equal the run's remainder with every remaining frame OF THAT LEVEL priced at
the measured rate and its slot-mates still free to follow the plan.

* old division (`EC_AV1_RL_NOREDIST=1`): `leaf target 658.415 is not the exact
  remainder 1034.458 (naive division would give 658.415)` — **FAILED**
* fixed: **ok** (1 passed)

Two more arms in the same test bound the carry: hidden levels spending 1 byte
each may not push the leaf target past 4x its opening share, and hidden levels
spending 40x may not push it below 0.25x (a zero target would stop the loop —
the regression above).

## 4. The gate, before and after

`bitrate_target_lands_within_5_percent_over_48_frames`, all four rates, pyramid
off and on. Before = base `8d9d056b` (`EC_AV1_RL_NOREDIST=1`, the old
division); after = `6da9cd06`.

| rate | pyramid | before | after |
|---|---|---|---|
| 384 kbit/s | off | 375164 bps (−2.3%) | 375164 bps (−2.3%) |
| 384 kbit/s | on | 396884 bps (+3.4%) | 392740 bps (**+2.3%**) |
| 768 kbit/s | off | 749204 bps (−2.4%) | 749204 bps (−2.4%) |
| 768 kbit/s | on | 768076 bps (+0.0%) | 768076 bps (+0.0%) |
| 1536 kbit/s | off | 1490612 bps (−3.0%) | 1490612 bps (−3.0%) |
| 1536 kbit/s | on | 1536020 bps (+0.0%) | 1536824 bps (+0.1%) |
| 2 Mbit/s | off | 1957592 bps (−2.1%) | 1957592 bps (−2.1%) |
| 2 Mbit/s | on | **1904600 bps (−4.8%)** | **2000760 bps (+0.0%)** |

Every flat arm is byte-identical (the flat path has one slot, no offsets and no
bound, and `replan` returns before any of this). Of the four pyramid arms, two
are byte-identical, one moves +1.1 points closer to target (384 kbit/s: the
`REPLAN_FLOOR`/`CLAMP` bound its late plan), and the clamped one is fixed. The
2 Mbit/s leaves now code 6370 B/frame against a 7100 B target held from frame
11 onward, instead of 5683 B against a target that only arrived at frame 43.

**The parked tree.** `hold/arftf-merge-c46e7005` (the ARF temporal filter at
strength 2) with both fix commits cherry-picked: 2 Mbit/s pyramid
**2001028 bps (+0.1%)**, against the −5.1% the defect report read there. All
eight of its arms pass (`hold-after.log`).

## 5. The rest of the gate — nothing else moved

The CQ streams carry NO `RateLoop` at all (`rate_loop: None` unless the caller
asks for `BytesPerFrame`/`Bitrate`), so every long-GOP/pin/preset stream is
untouched by construction, and the runs confirm it:

* `encode::tests::the_encoders_own_streams_are_byte_identical_to_their_pins`
  ok — pins `(150, 8218, 0xd533c8ea14caae6e)` and
  `(60, 33017, 0xbcedb694dcfe9146)` unchanged, not re-taken.
* `encoder::tests::every_speed_preset_decodes_sample_exact_through_both_decoders`
  (`--ignored --exact`): 1 passed.
* split suite, `cargo test -p ec-av1 --lib`: s1 (`--skip stream::`) 344 passed
  0 failed 34 ignored RC=0; s2 (`stream:: --skip 10bit`) 202 passed 0 failed
  15 ignored RC=0; s3 (`10bit`) 42 passed 0 failed 1 ignored RC=0.
* `cargo check --workspace --all-targets -j4`: RC=0, 0 errors, 0 ec-av1
  warnings.

EVIDENCE: $HOME/.cache/rlclamp/{bitrate-before,bitrate-after,bitrate-after2,hold-after,s1,s2,s3,preset,check}.log |
EC_AV1_RL_CENSUS=1 [EC_AV1_RL_NOREDIST=1] cargo test -p ec-av1 --lib bitrate_target_lands --nocapture, then the split suite + preset + workspace check |
2 Mbit/s pyramid 1904600 → 2000760 bps (−4.8% → +0.0%), 8/8 arms inside ±5%, pins unchanged, 588 tests green

## 6. Knobs this lane leaves

* `EC_AV1_RL_CENSUS=1` — the per-frame rate-loop census above.
* `EC_AV1_RL_NOREDIST=1` — the OLD division, kept as the before/after arm: it
  is what makes every number in this report reproducible in one binary.

## 7. Open

* The leaf level has its own floor that this fix does not name: `q[slot]` is
  clamped to 0, so a leaf's effective quantizer cannot go below
  `leaf_q_offset` (12) and `bound` — which reads the coder's 1..=255 clamp —
  never fires for it. It cost nothing here (the leaves reached 6.4 kB/frame at
  `eff_q` 21-39, far off that floor), but a target much past 2.4 Mbit/s on this
  clip would hit it, and then no redistribution is left: every level is at its
  floor and the encoder simply cannot spend more.
