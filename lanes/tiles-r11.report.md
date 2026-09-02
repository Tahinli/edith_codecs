# lane-tiles r11 — multi-tile gates with the tools ON; the films are single-tile

## Charter premises, re-measured first (both stale)
1. **"all 45 real-aomenc gates are single-tile"** — false at main 9c35ecc. This lane
   already ran 10 rounds, all merged; `crates/ec-av1/src/stream.rs` carries 9 multi-tile
   gates (2/4 tile columns, 2/4 tile rows, an inter one, several tile-group OBUs,
   non-uniform spacing). The branch was already identical to main at start
   (`git log main..HEAD` empty).
2. **"round 1"** — this is r11.

What was genuinely missing: every one of those 9 gates turns the coding tools OFF
(`--enable-rect-partitions=0 --enable-ab-partitions=0 --enable-cdef=0
--enable-restoration=0`, some `--loopfilter-control=0`, `--min/--max-partition-size=32`).
That is the [[tool-disabled-in-every-gate]] shape: a tile-edge defect inside any of those
tools was invisible to all nine.

## HIS FILMS' REAL TILING (the charter's decision input)
Both films are **one tile**: `cols=1 rows=1 uniform_spacing=1 context_update_tile_id=0`,
constant over 181 parsed frame headers each. Cross-checked with a foreign parser
(ffmpeg `trace_headers` bsf): `tile_cols_log2 = 0`, `tile_rows_log2 = 0`,
`uniform_tile_spacing_flag = 1` on every frame. **Multi-tile is not on the critical path
for his library**; it stays a conformance obligation, not a film blocker.

EVIDENCE: /tmp/.../scratchpad/troyB.obu, hungerB.obu (120 frames each, `-ss 300 -c:v copy -f obu`) | `cargo run -p ec-av1 --example decode_probe -- <obu>` + `ffmpeg -v trace -bsf:v trace_headers` | ours: 181 headers, one distinct tiling (1x1); ffmpeg: tile_cols_log2=0 tile_rows_log2=0 on every frame; Troy 1920x792, Hunger 3840x1608

## New gate family (`crates/ec-av1/src/stream.rs`, `run_multi_tile_gate` + 4 arms)
256x128 (four 64x64 SBs wide, two tall), through `decode_stream` — the same entry point a
caller decoding a file uses, not the tile function the older gates call directly.
Recipe: `--enable-rect-partitions=1 --enable-ab-partitions=1 --enable-cdef=1
--enable-restoration=1`, deblocking left at its default so it filters across tile
boundaries, `--sb-size=64`, `--threads=1 --row-mt=0`, 6 seeds.
Hard asserts: parsed `tile_info.cols*rows == expected` on every non-`show_existing_frame`
header, and `tile_hits` delta `>= tiles * frames` (a header claiming 4 tiles cannot pass on
one decoded tile).

| arm | grid | depth | frames | result |
|---|---|---|---|---|
| `a_real_aomenc_multi_tile_intra_stream_decodes_pixel_exact` | 2x2 | 8 | 1 | 5/6 exact, 1 named refusal |
| `a_real_aomenc_multi_tile_intra_10bit_stream_decodes_pixel_exact` | 4 cols | 10 | 1 | 5/6 exact, 1 named refusal |
| `a_real_aomenc_multi_tile_inter_10bit_stream_decodes_pixel_exact` | 4 cols | 10 | 16 | 3/6 exact, 3 named refusals |
| `a_real_aomenc_multi_tile_inter_stream_decodes_pixel_exact` | 2x2 | 8 | 16 | **`#[ignore]`d — open defect, below** |

The two inter arms additionally pass `--max-partition-size=32 --enable-tx-size-search=0`:
without them every attempt refuses on two *pre-existing, non-tile* limits (the inter path
only recurses a superblock as SPLIT; it reads no `tx_depth` under `TxMode::Select`), so the
gate would compare no tile edge at all. Stated here rather than silently folded in.

EVIDENCE: cargo test output | `EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib multi_tile -- --nocapture --test-threads=1` | 3 arms pass with tools on, 4th ignored

## OPEN DEFECT (fix-now was not reached; deferred)
Seed 42, 2 tile rows: one 32x32 luma block at **x[224..255] y[32..63]** (tile row 0, frame
right edge) mismatches ffmpeg from **frame 4**, propagating through inter prediction to
12 of 16 frames. Localisation and ablations:
- bbox with deblocking on: x[221..255] y[30..69] — crosses the tile row boundary (y=64), so
  deblock only *spreads* it; with `--loopfilter-control=0` it collapses to the single 32x32
  block above.
- NOT removed by `--enable-restoration=0`, `--enable-cdef=0`, `--loopfilter-control=0`.
- Reproduces with 2 tile rows *and* with the 2x2 grid; seeds 47 and 48 decode 16/16 exact
  with two tile rows, so it is content-dependent, not a systematic tile-row defect.
- **Tile attribution is UNPROVEN**: the same-content single-tile control re-encodes to
  different partitions and refuses ("an inter SB-level partition type other than SPLIT"), so
  no one-tile comparison of this content exists ([[gate-recipe-confound]]). Calling it a
  tile-edge defect would be a guess.
Reproducer: `EC_AV1_GATE_DUMP=/tmp/mt.obu cargo test -p ec-av1 --lib a_real_aomenc_multi_tile_inter_stream -- --ignored`.
Disposition: **deferred(a decodable single-tile control of the same content — unblocked by
either the inter SB-level non-SPLIT partition capability or an aomenc recipe that keeps
partitions identical across tile settings)**.

EVIDENCE: /tmp/.../scratchpad/{c11,c01,norest,nocdef,nolf}.obu + cmp.py bboxes | encode seeds 42..48 x tile-rows 0/1 x {restoration,cdef,deblock} off, decode via decode_probe, per-pixel diff vs `ffmpeg -f rawvideo` | seed42 rows=1: 12/16 frames mismatch, nolf bbox x[224..255] y[32..63]; seed47 rows=1 and seed48 rows=0/1: 0/16 mismatch

## Neighbour-map sweep (COMMON's NEIGHBOUR MAPS paragraph)
`grep -n "> 0" `-swept the per-mi availability guards: `decode.rs` uses
`tile_row0_mi`/`tile_col0_mi` (fields at decode.rs:2420-2421, reset in
`Neighbours::start_tile`, called per tile at decode.rs:5549 and 10512) and `PlaneBuf`'s
reach clamps use `tile_x0/tile_y0`; no `> 0` availability guard remained on a per-mi side
band. Nothing to fix — recorded so the next lane does not re-sweep it.

## gate_coverage (charter item 3)
- Tiling **cannot** join the derivation and needs no entry: the check keys on
  `--enable-<tool>=0/1`, and a `--tile-columns=` presence check would read `=0` (one tile)
  as coverage. Noted in `gate_coverage.rs` with the gates that do spell it.
- Deleted 5 stale `NEVER_EXERCISED_10BIT` entries. Two of them
  (`enable-global-motion`, `enable-dist-wtd-comp`) were closed by lane-cwarp's 10-bit
  compound-warp gate without updating this list, **so `never_exercised_10bit_matches_the_gate_recipes`
  was already failing at main 9c35ecc** — a pre-existing red, fixed here. The other three
  (`enable-ab-partitions`, `enable-rect-partitions`, `enable-restoration`) are closed by the
  new 10-bit arms.
- Also fixed the same body-split shape in my own helper: `gate_coverage` splits gate bodies
  on `\n    #[`, so a bare helper `fn` is glued onto the preceding gate's recipe.
  `run_multi_tile_gate` carries `#[track_caller]` so it owns its body.

## Test totals
`EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib -- --test-threads=2`:
**282 passed, 0 failed, 26 ignored** (1211 s). `cargo check -p ec-av1 --tests`: clean.
(A first run at default test-threads was SIGTERMed by `scripts/memguard-runner.sh` under
10 concurrent lanes; `--test-threads=2` is the working setting, not a result.)

## Files changed
- `crates/ec-av1/src/stream.rs` — `run_multi_tile_gate` + 4 arms.
- `crates/ec-av1/src/gate_coverage.rs` — 5 stale 10-bit entries deleted, tiling note.
- `crates/ec-av1/examples/decode_probe.rs` — prints every frame header's tiling.

## Commit
- 2e73186 on `lane-tiles`. Never pushed, never merged, main untouched.

## Coordinator items received mid-round (after the r11 commit)
1. **Palette left neighbour has no tile guard — CONFIRMED and FIXED.**
   `decode.rs` `palette_ctx_and_cache` and `palette_uv_cache` read
   `left_palette_size[r]` / `left_palette_uv_size[r]` with **no availability guard at
   all**, where libaom's `av1_get_palette_cache` takes `xd->left_mbmi`, NULL at the
   block's own tile left column. Fixed with `let left_ok = c > self.tile_col0_mi / (SUB / MI);`
   on both (`at` is in SUB units, so the mi-unit tile origin is divided the same way
   `start_tile` divides `col0_mi` for `above_mode`). `start_row` also never cleared
   `left_palette_*`/`left_palette_uv_*`; it does now (COMMON's NEIGHBOUR MAPS rule 2).
   Status: **code-verified** — 54 tile tests + gate_coverage green, no gate yet proves it.
2. **Palette + screen-content multi-tile arm — written, `#[ignore]`d, NOT yet proof.**
   `a_real_aomenc_multi_tile_palette_stream_decodes_pixel_exact` (smptebars, 2 tile
   columns, `--tune-content=screen --enable-palette=1`, asserts `palette_hits` rose).
   It currently refuses on every seed before a palette block reconstructs, because the
   per-arm `extra` flags do NOT override the base recipe: **aomenc keeps the FIRST
   occurrence of a repeated `--enable-*` flag** (measured — appending
   `--enable-rect-partitions=0` after the base `=1` changed the refusal not at all).
   Fix next round: move `args.extend(extra)` to precede the base list in
   `run_multi_tile_gate`. Disposition: **deferred(one edit — extra before base)**.
3. **lane-ab16's `--tile-columns=1` mismatch (mandelbrot 96x96, x=32..63 y=80..95)** —
   NOT reproduced this round. Disposition: **deferred(tool-call cap)**.
4. **Frame-relative `mi_row > 0` / `mi_col > 0` guards at decode.rs ~8547-8548 and
   mvstack.rs ~727,733,742,756,1486,1492,1501,1515** — NOT swept. mvstack has no tile
   origin plumbed in at all, so this is a signature change plus a call-site sweep, and
   it needs a multi-tile INTER gate to show — the arm that is `#[ignore]`d above for an
   unrelated open defect. Disposition: **deferred(next round; needs tile bounds threaded
   into mvstack, then the inter multi-tile arm re-enabled to measure)**.
