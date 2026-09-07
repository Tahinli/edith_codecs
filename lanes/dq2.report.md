# lane-dq2 — delta_q at presets 3/6 with a real tpl window, and tpl depth at the long GOP

Scope: measurement + `speed.rs` constants (`DELTAQ_RES`, `TPL_DEPTH`) only.

Env forms in use: `EC_AV1_SPEED=<n>`, `EC_AV1_DELTAQ=2` (delta_q_res log2, `4`/absent = off),
`EC_AV1_TPL_D=<pictures>` (encode.rs:9371 `tpl_depth()` — the depth override exists, spelled
`EC_AV1_TPL_D`, not `EC_AV1_TPL_DEPTH`).

## 1. delta_q vs control, 12-frame `bd_rate_screen_native` (BD vs libaom / vs rav1e)

(filled in as arms land)

## 2. long-GOP tpl depth, preset 0

## 3. Shipped constants
