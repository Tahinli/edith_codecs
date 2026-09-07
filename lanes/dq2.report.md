# lane-dq2 — delta_q at presets 3/6 with a real tpl window, and tpl depth at the long GOP

Scope: measurement + `speed.rs` constants (`DELTAQ_RES`, `TPL_DEPTH`) only.

Env forms in use: `EC_AV1_SPEED=<n>`, `EC_AV1_DELTAQ=2` (delta_q_res log2, `4`/absent = off),
`EC_AV1_TPL_D=<pictures>` (encode.rs:9371 `tpl_depth()` — the depth override exists, spelled
`EC_AV1_TPL_D`, not `EC_AV1_TPL_DEPTH`).

## 1. delta_q vs control, 12-frame `bd_rate_screen_native` (BD vs libaom / vs rav1e)

All arms: `bd_rate_screen_native` on the prebuilt release binary, 12 frames, gop 12,
1 tile, native crops. Rows keyed by the log's own header line.

### preset 3 (`EC_AV1_SPEED=3`, tpl depth 4)

| arm | bars 1080p | bars 2160p | film A | film B | screen |
|---|---|---|---|---|---|
| control | +43.1 / +20.4 | +49.2 / +19.0 | +22.3 / -4.1 | +27.3 / -0.3 | +33.4 / -23.2 |
| `EC_AV1_DELTAQ=2` | +40.2 / +18.8 | +49.5 / +19.6 | +22.0 / -4.1 | +27.9 / +0.4 | +33.8 / -23.1 |

Control reproduces lane-tplwin's preset-3 row to the digit on all three real-content rows.
VERDICT preset 3: **reject**. Film A moves -0.3 / 0.0 (below the 0.5 the keep rule wants from a
single row), film B moves +0.6 / +0.7 WORSE on both columns, and screen is +0.4 worse vs libaom
(over the 0.3 bound). The two bars rows are fixtures, never a decision.

### preset 6 (`EC_AV1_SPEED=6`, tpl depth 4)

| arm | bars 1080p | bars 2160p | film A | film B | screen |
|---|---|---|---|---|---|
| control | +68.4 / +40.0 | +62.7 / +28.0 | +27.2 / -0.2 | +35.2 / +6.6 | +34.8 / -22.3 |
| `EC_AV1_DELTAQ=2` | +67.0 / +40.2 | +64.1 / +29.5 | +26.6 / -0.4 | +34.7 / +6.4 | +35.3 / -22.2 |

Control reproduces the charter's preset-6 row to the digit.
VERDICT preset 6: **both film rows pass, the screen row fails the rule.** Film A -0.6 / -0.2 and
film B -0.5 / -0.2, i.e. both real films improve on both columns -- the first arm in which
delta_q is a win on film at all, and the tpl window is what made it one. Screen capture is +0.5
worse vs libaom (bound is 0.3), so under the keep rule as written `DELTAQ_RES` does NOT ship on
at preset 6 either.

The shape is the one lane-b64b already met: a lever that pays on film and loses on the capture.
The fix there was a `!screen` content gate on the tool. That gate lives in `encode.rs`
(`deltaq_res_log2`), outside this lane's scope -- see the deferred list.

## 2. long-GOP tpl depth, preset 0

`bd_rate_film_long_gop` (48 pictures, gop 48, the two film rows only), `EC_AV1_SPEED=0`.
The depth override EXISTS: `encode.rs:9371 tpl_depth()` reads `EC_AV1_TPL_D` (`>= 1`), which wins
over `speed::TPL_DEPTH`.

| arm | film A | film B |
|---|---|---|
| control (depth 8, shipped) | +26.4 / -6.6 | +89.8 / +9.1 |
| `EC_AV1_TPL_D=4` | +26.4 / -6.6 | +89.8 / +9.1 |

Both rows land on the same tenth on both columns, and the wall is inside noise (film A 577.0s vs
575.0s, film B 514.8s vs 510.0s ours). The byte points differ slightly (film B q=5 71161 vs
70641 B), so the arms really did code different streams -- the depth is simply not worth a tenth
of a BD point over a 48-picture GOP.

That does NOT reopen preset 0's shipped 8: the 12-frame gate measured depth 4 costing film B
0.7 on BOTH columns there, and this arm measures no long-GOP win to trade for it. `TPL_DEPTH`
ships unchanged.

## 3. delta_q over a long GOP at preset 6

`bd_rate_film_long_gop` (48 pictures, gop 48), `EC_AV1_SPEED=6`. Run because preset 6 is where
delta_q pays on film, and the long GOP is the shape his exports actually have (the screen row is
not in this gate).

| arm | film A | film B |
|---|---|---|
| control | +32.8 / -2.7 | +104.5 / +18.3 |
| `EC_AV1_DELTAQ=2` | +32.0 / -2.9 | +104.3 / +17.8 |

Both films improve on both columns again (film A -0.8 / -0.2, film B -0.2 / -0.5), i.e. the
12-frame film result is not a 12-picture artefact -- it holds, slightly larger on film A, over 48
pictures. delta_q is a real film lever at preset 6 once the tpl map exists; the only thing
standing between it and the default is the capture row.

## 4. Shipped constants

**Nothing changes.** `speed::DELTAQ_RES` stays `[4; 11]` (off at every preset) and
`speed::TPL_DEPTH` stays `[8, 8, 8, 4, 4, 4, 4, 1, 1, 1, 1]`. No source file outside
`lanes/dq2.report.md` is touched by this lane.

* preset 3 delta_q: rejected on its own numbers (film B worse on both columns).
* preset 6 delta_q: passes the film half of the keep rule twice (12-frame and long-GOP), fails
  the screen bound by 0.2 over it. Shipping it on would trade 0.5 BD points of his capture
  content for 0.5 of his film content -- the rule's answer is no, and the honest fix is a
  content gate, not a looser rule.
* `TPL_DEPTH` at preset 0: depth 4 and depth 8 are indistinguishable over 48 pictures, so the
  12-frame measurement (which prefers 8 by 0.7 on film B) remains the only signal and 8 stays.

## Deferred

* `deferred: delta_q behind a !screen content gate at preset 6 --` the measurement says it wins
  on both films on both gates and loses only on the capture, exactly lane-b64b's shape; the gate
  itself lives in `encode.rs deltaq_res_log2` (plus a `DELTAQ_RES` flip at preset 6), which is
  outside this lane's `speed.rs`-only scope -- `unblocked by a charter that may edit encode.rs`.
* `deferred: presets 1, 2, 4, 5 for delta_q --` unmeasured; preset 3 rejects and preset 6 is
  screen-blocked, so the neighbours only matter once the content gate exists --
  `unblocked by that same lane`.
* `deferred: long-GOP tpl depth above preset 0 --` only preset 0 was run; presets 3..6 already
  ship 4 and the preset-0 arm shows the axis is flat over a long GOP -- `unblocked by a lane that
  finds a reason to care`.
