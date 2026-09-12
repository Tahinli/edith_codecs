# lane-tplfut-arf — charter: the tpl future map at the ARF (recon: the argument already changed)

`edith_codecs-tplfut`, branch `lane-tplfut-arf`, base `d27a904d`.
The lane-arffilt reconciliation (`lanes/arffilt.report.md`, merged `b95763a8`)
names as its third open lever: "the tpl future map at the ARF (the buffered
future pictures reach the source filter only, not the temporal lambda map) —
now a one-argument change per lane-lookahead §5".

## 1. Recon verdict — the lever SHIPPED before that line was written

`lane-lookahead` §5's deferred item ("encode_pyramid_inter still gets
`tpl_window(pos, true)`, the display-past leaves ... now a one-argument change
(pass the same future slice as lookahead)") was implemented and tuned by two
lanes that both merged BEFORE lane-arffilt's base `7db5094a`:

| prior lane | commits | merged | what it shipped |
|---|---|---|---|
| lane-tplfut | `9ed92a1d`, `112893ff` | `daed0265` | `code_group` builds the top ARF's tpl lambda window from the buffered next group (`arf_tpl`, modes 0/1/2/3, `EC_AV1_TPL_FUT`); mode 2 (past then future, nearest-first) ships default ON: `speed::TPL_FUT = [2; 11]` |
| lane-tplhalf | `0c8d5778`, `600dfa34` | `aab48896` | mode 2's split (`TPL_FUT_HALF = [2,2,2,1; 1…]`) and budget (`TPL_FUT_WIN = [11,11,11,0; 0…]`) — 2 past + every buffered future picture, saturating the one-group lookahead |

`git merge-base --is-ancestor daed0265 7db5094a` and `aab48896 7db5094a` both
hold: **the arffilt deferred line is a stale pointer** — it quotes
lane-lookahead §5 verbatim but was written at a base that already contains the
change it asks for. Class: `stale-deferred-pointer` (an open-levers list
reconciled against one lane's §5 instead of the current tree).

## 2. The exact one-argument change, as it exists at `d27a904d`

`encoder.rs::Av1Encoder::code_group(group, future)` — `future` is the next
group's sources buffered by lane-lookahead's `ready` slot:

* pre-tplfut, the top ARF's `encode_pyramid_inter` lookahead argument was
  `tpl_window(group.len() - 1, true)`: the group's own display-past sources
  reversed (the top ARF is its group's LAST picture in display order);
* now (`encoder.rs`, the `arf_tpl` block closing at the `encode_pyramid_inter`
  call for `group.last()`): mode 0 / empty-future keeps exactly that past
  window; mode 2 (shipped) takes `past()` truncated to `tpl_fut_half()` (= 2)
  and extends it with `arf_tpl_future(budget − past_len)` off the buffered
  future, budget `tpl_fut_win(depth)` (= 11) — i.e. 2 past + every available
  future picture of the next group, nearest-first.

Shipped constants (`speed.rs`): `TPL_FUT = [2; 11]`, `TPL_FUT_HALF =
[2, 2, 2, 1, …]`, `TPL_FUT_WIN = [11, 11, 11, 0, …]` (presets 0..=2 carry the
swept values; 3..=6 keep `TPL_DEPTH=4`-era 1/0, unswept there).

The OFF state of this lever is `EC_AV1_TPL_FUT=0` and it is bit-exact BY
CONSTRUCTION: mode 0's arm is the literal pre-lever expression
(`(0, _, _) | (_, true, _) => past()`), and with no buffered future every mode
falls back to it. A second, default-OFF `EC_AV1_TPLFUT_ARF` knob beside the
shipped `EC_AV1_TPL_FUT` would be a second convention beside an existing one
— refused on the lane-arffilt precedent (its merge message records the same
refusal for `EC_AV1_ARFFILT`).

## 3. Prior art's gate evidence (controls RUN, not quoted, in those lanes)

48-picture `bd_rate_film_long_gop`, BD vs libaom cpu-used 6 / rav1e speed 6:

| arm | film A | film B |
|---|---|---|
| mode 0, past-only (tplfut control) | +21.3 / −8.8 | +72.1 / −1.2 |
| mode 2, 3 past + 4 future (tplfut ships) | +21.2 / −8.8 | +71.6 / −1.5 |
| mode 2, 2 past + 5 future (tplhalf) | +20.9 / −9.0 | +71.0 / −1.8 |
| mode 2, budget 11 (tplhalf ships = standing) | **+20.9 / −9.0** | **+70.4 / −2.2** |

lane-l2's control at `7db5094a` re-read **+20.9/−9.0 / +70.4/−2.2 to the
digit** — the standing table IS this lever's shipped state. Cumulative lever
delta (mode 0 → shipped): film B −1.7/−1.0, film A −0.4/−0.2, i.e. the keep
rule (one film row ≥0.5 down, the other not worse than +0.3) cleared on the
merged evidence.

`bd_rate_screen_native` is STRUCTURALLY BLIND to this lever at 12 pictures:
`group_target` absorbs the tail into ONE group of 11, no next group is ever
buffered, `future` is empty, every mode takes the past window — proven by
lane-lookahead §3 ("byte-identical to its control on every row") and
lane-tplfut §4, and re-proven on THIS base by this lane's local probe
(report §2).

## 4. What THIS lane therefore does (and refuses)

1. Records this reconciliation (this charter) so nobody rebuilds the shipped
   lever or re-opens its swept axes (mode, split, budget — all bracketed).
2. Re-proves the witnesses on THIS base at `d27a904d` (five merges past the
   last direct measurement): byte pins `(150, 8291, 0x1f00bb0eb099a27f)` /
   `(60, 33227, 0x57ee6b1f8eacd881)` at the default and at `EC_AV1_SPEED=6`;
   `the_future_tpl_window_still_codes_every_picture_and_moves_the_stream`
   (stream MOVES when the window engages, every picture coded, sample-exact
   through `decode_stream` AND ffmpeg); the 12-picture blindness probe.
3. Measures the lever's CURRENT contribution on VPS-3 (`tCloud@178.105.165.182`,
   unused so far), one gate at a time per the managed skill:
   `bd_rate_film_long_gop` control (`EC_AV1_TPL_FUT=0`) vs arm (the shipped
   default — the decisive pair on a base that has moved), and
   `bd_rate_screen_native` control at the default for the five standing rows
   (the arm there is byte-identical by construction — the probe stands in for
   a fourth gate). LOADAVG pairs per gate.
4. Keep rule on the fresh pair: one film row ≥0.5 down, the other not worse
   than +0.3, screen not worse 0.3, wall ≤+15%. Met → disposition "stays
   default ON" (already shipped) + pins at default and `EC_AV1_SPEED=6` +
   three-lane lib suite (fixtures symlinked) + `cargo check --workspace
   --all-targets -j4`. Not met → flip `TPL_FUT` to 0 at presets 0..=2 with the
   evidence, document.

No merge, no push.
