# lane-tplfut-arf — RECONCILED: the tpl future map at the ARF was already shipped; this lane re-proves it and re-measures it on the current base

Worktree `edith_codecs-tplfut`, branch `lane-tplfut-arf`, base `d27a904d`.
The assignment named lane-arffilt's deferred lever ("the tpl future map at
the ARF ... now a one-argument change per lane-lookahead §5") and asked for
it behind a new default-OFF `EC_AV1_TPLFUT_ARF` knob.

**Recon verdict (charter, `lanes/tplfutarf.charter.md`, commit `f606fb57`):
no code change exists to make.** The exact one-argument change — pass the
buffered next group to the top ARF's temporal-lambda window instead of the
past-only `tpl_window(group.len()-1, true)` — was implemented by
lane-tplfut (`9ed92a1d`, `code_group`'s `arf_tpl` block,
`encoder.rs` ~L2257-2291 at this base) and tuned by lane-tplhalf
(`0c8d5778`), both merged ancestors of lane-arffilt's OWN base `7db5094a`
(`git merge-base --is-ancestor daed0265/aab48896 7db5094a` holds):
arffilt's deferred line is a stale pointer (`stale-deferred-pointer`), and
a second default-OFF knob beside the shipped `EC_AV1_TPL_FUT` is refused on
the lane-arffilt precedent — mode 0 IS the bit-exact off
(`(0, _, _) | (_, true, _) => past()` is the literal pre-lever expression).
Shipped constants at this base: `TPL_FUT = [2; 11]`,
`TPL_FUT_HALF = [2, 2, 2, 1; 1…]`, `TPL_FUT_WIN = [11, 11, 11, 0; 0…]`
(2 past + every buffered future picture; saturates the one-group lookahead).

What the lane therefore shipped is this reconciliation plus a decisive
re-measurement of the lever's CURRENT contribution on a base that has moved
five merges past lane-l2's last confirming control, on VPS-3
(`tCloud@178.105.165.182`, first use by any lane) per the managed skill
`ec-av1-vps-gate-runners`. Unknobbed, per the assignment's own escape hatch,
with the measurement made decisive.

## 1. Witnesses, re-proven on THIS worktree at `d27a904d` (tree byte-identical to HEAD; probe added and removed)

| witness | result |
|---|---|
| `encode::tests::the_encoders_own_streams_are_byte_identical_to_their_pins` | PASS — pins `(150, 8291, 0x1f00bb0eb099a27f)`, `(60, 33227, 0x57ee6b1f8eacd881)` hold |
| same at `EC_AV1_SPEED=6` | PASS (8.6 s) |
| `encoder::tests::the_future_tpl_window_still_codes_every_picture_and_moves_the_stream` | PASS (52.2 s) — the stream MOVES when the window engages (modes 1/3 vs 0 at 17 pictures), coding order/levels unchanged, every picture decoded sample-exact through `decode_stream` AND ffmpeg (the engagement + both-decoder guard) |
| `encoder::tests::the_lookahead_holds_one_group_and_still_codes_every_picture` | PASS (16.2 s) |
| `encoder::tests::every_speed_preset_decodes_sample_exact_through_both_decoders` (`--ignored`) | PASS (18.4 s) |
| THROWAWAY `tplfut_twelve_picture_blindness_probe` (run once, then CUT) | PASS — at the native gate's shape (12 frames, `gop = 12`) modes 0/1/2/3 produce BYTE-IDENTICAL streams |

## 2. The 12-frame native gate is structurally blind — the arm gate is its control, and was not run

`native_bd_arm` sets the sequence's `gop = frame count`: at 12 frames the
gopad tail rule forms ONE group of 11, no next group is ever buffered,
`future` is empty, and every `TPL_FUT` mode takes the past window. Proven
before by lane-lookahead §3 and lane-tplfut §4 and re-proven here by the
probe above. The probe's first run was a FALSE RED — 12 frames at
`gop 32` truncates mid-GOP into `[8, 3]`, the first group's ARF DOES get a
buffered future, and modes 1-3 really do move that stream (class:
`gate-shape-mismatched-probe`; the gate never runs that shape). Conclusion:
`bd_rate_screen_native` needs only the CONTROL run; its five rows are the
arm's rows byte for byte.

## 3. Gates on VPS-3 — the deciding pair, control vs arm on the current base

One gate at a time, sequential chain (`tplfut-chain` user unit, per-gate
logs `~gates/tplfut-{lgc,lga,nat}.log`, chain log `~gates/chain-tplfut.log`),
warm-compiled `target-tplfut`, `fixtures` symlink, `.git` stub `git init`-ed.
All three gates EXIT=0. `bd_rate_film_long_gop`: 48 pictures, gop 48,
BD-rate vs libaom cpu-used 6 / vs rav1e speed 6, lower is better.

| gate | film A | film B | wall A (ours) | wall B (ours) | LOADAVG before → after |
|---|---|---|---|---|---|
| control `EC_AV1_TPL_FUT=0` | +21.2% / −8.9% | +71.4% / −1.6% | 1197.1s | 940.0s | 0.10 0.29 0.36 → 1.06 1.07 1.01 |
| arm (shipped default, mode 2) | **+20.9% / −9.0%** | **+70.4% / −2.2%** | 1193.3s | 933.0s | 1.06 1.07 1.01 → 1.26 1.17 1.13 |
| **Δ = the lever's current contribution** | **−0.3 / −0.1** | **−1.0 / −0.6** | −0.3% | −0.7% | |

The arm reproduces the standing table (lane-tplhalf's +20.9/−9.0,
+70.4/−2.2; re-read at +20.9/−9.0, +70.4/−2.2 by lane-l2's control on
`7db5094a`) TO THE DIGIT on VPS-3. The control reproduces lane-tplfut's
mode-0 row (+21.3/−8.8, +72.1/−1.2) to within 0.1-0.7 — the residual is the
five merged encoder waves since, direction favourable.

`bd_rate_screen_native` control (12 pictures, all five rows) — the arm is
byte-identical by §2, so these ARE the arm rows:

| row | BD vs libaom | BD vs rav1e | wall ours | standing |
|---|---|---|---|---|
| bars 1080p | −3.3% | −19.0% | 360.6s | −3.3 / −19.0 |
| bars 2160p | +8.4% | −13.9% | 306.5s | +8.4 / −13.9 |
| film A | +17.9% | −6.3% | 318.3s | +17.9 / −6.3 |
| film B | +22.6% | −3.8% | 272.4s | +22.6 / −3.8 |
| screen capture | +13.7% | −33.6% | 205.8s | +13.7 / −33.6 (intertx's documented delta) |

All five rows to the digit. Gate walls: lgc 44m25s, lga 44m09s, nat 29m44s;
no sibling load on the box (idle before each gate; the box's one prior
incident — the whole user manager dying with the launch ssh session at
6m38s, killing chain+gate cleanly — is a provisioning lesson now in the
managed skill: `loginctl enable-linger`).

## 4. Keep rule and disposition

Keep rule (arm vs control): one film row ≥0.5 down — film B −1.0/−0.6 on
both columns ✓; other flat — film A −0.3/−0.1, not worse ✓; screen not worse
0.3 — 0.0 by construction (§2) and the five control rows stand ✓; wall ≤+15%
— −0.3%/−0.7% ✓. **MET.**

**Disposition: the lever STAYS default ON** (`TPL_FUT=2` with
`HALF=2`/`WIN=11` at presets 0..=2), exactly as lane-tplfut + lane-tplhalf
shipped it. No knob is added; no code changes; the lane's commits are
docs-only (charter + this report). Met-branch actions, all run:

* pins re-run at the default AND at `EC_AV1_SPEED=6`: hold (8291 / 33227);
* three-lane `ec-av1` lib suite off one release binary (fixtures symlinked):
  **348 + 202 + 42 = 592 passed / 0 failed** (44 / 15 / 1 ignored — the
  ignored count grew 56 → 60 across lanes intertx..vp8, no failures);
* `cargo check --workspace --all-targets -j4`: 0 errors, 0 `ec-av1`
  warnings; two pre-existing `ec-vorbis` oracle warnings at this base (the
  known oracle doc-warning plus its dead `decode_capture` wrapper in
  `tests/oracle.rs` — untouched by this lane, named here so it is not
  rediscovered).

## 5. What this lane did NOT do

* No encoder change (reconciliation; see the charter for the refusal).
* `deferred: TPL_FUT/HALF/WIN at presets 3..=6` — unswept by lane-tplhalf,
  untouched here (long-GOP arms would be needed).
* `deferred: per-block weight refinement in arf_temporal_filter` and the
  `deferred: top-ARF mode/partition gap` and `deferred: two-pass-like
  allocation over the buffered group` — the other open levers the arffilt
  reconciliation names; none of them is this lever.

Pins: `(150, 8291)`, `(60, 33227)` — confirmed on this worktree.
No merge, no push.
