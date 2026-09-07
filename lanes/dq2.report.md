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

## 3. Shipped constants
