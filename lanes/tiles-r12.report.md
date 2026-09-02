# lane-tiles r12 — the palette gate is live; the refusal it hit was a filter-intra desync

## Charter item 1: REJECTED, with the measurement (do NOT move `extend(extra)`)
The charter (and r11, and a ledger `constraint|` line) states "aomenc keeps the FIRST
occurrence of a repeated flag". **Measured false.** Same input, one frame, 2 tile columns:

| args | md5 | size |
|---|---|---|
| `--enable-palette=0 --enable-palette=1` | 34b7fe63… | 299 |
| `--enable-palette=1` alone              | 34b7fe63… | 299 |
| `--enable-palette=1 --enable-palette=0` | f8339ae1… | 334 |
| `--enable-palette=0` alone              | f8339ae1… | 334 |

The LAST occurrence wins (libaom `aomenc.c` updates the existing entry in
`config->arg_ctrls` in place). The current order — base list, then `extra` — is already
correct; moving `extra` first would silently disable every per-arm override. Not done, and
the ledger line is corrected below.

EVIDENCE: /tmp/.../scratchpad/p01.obu,p10.obu,p0.obu,p1.obu | aomenc same y4m, 4 flag orders | md5(p01)==md5(p1), md5(p10)==md5(p0)

## Root cause of the palette arm's refusal: `use_filter_intra` read after a palette block
With the overrides proven to arrive, the palette arm still refused — and the refusal was a
**correlate, not the defect** ("a HORZ_A/… partition below 16x16" with rect/ab OFF is
impossible; the stream had desynced). Isolation, single tile, identical recipe:
`--enable-palette=0` → `OK: 1 frames decoded`; `--enable-palette=1` → refusal.

Range ladder (instrumented aomdec `EC_TRACE_MODE_STEP` vs ours) diverges inside the first
16x16 palette block, mi(0,8): both sides agree through `angle_uv` (rng=38738); ours then
reads `use_filter_intra=1` (rng→61700) while libaom reads no such symbol.
`av1_filter_intra_allowed` (blockd.h) requires
`mbmi->palette_mode_info.palette_size[0] == 0` — **libaom writes no `use_filter_intra`
symbol for a Y-palette block**, and we consumed one, desyncing the rest of every
screen-content tile from its first palette block on.

Fix: `crates/ec-av1/src/decode.rs:5120` — `&& palette_y_pending.is_none()` on the
`use_filter_intra` read.

Class sweep (`filter_intra_size_class*` call sites): the only other reader is
`read_intra_mode_rect` (decode.rs:3661), which refuses screen content wholesale before any
palette syntax, so no palette block can reach it; the two inter-frame intra sites
(decode.rs:12009, 13109) return `unsupported` the moment palette fires. One site, fixed.

EVIDENCE: /tmp/.../scratchpad/{w0,w1}.obu + ref2.txt/our2.txt | aomenc palette=0/1, same recipe; EC_TRACE_MODE_STEP ladder both decoders | w0 OK / w1 refused before; both OK after; first divergence mi(0,8) rng 38738: oracle no symbol, ours use_filter_intra=1

## Charter item 2: `a_real_aomenc_multi_tile_palette_stream…` un-ignored and GREEN
- `#[ignore]` removed; **6/6 attempts pixel-exact vs ffmpeg, 0 refusals**.
- Per-attempt proof added: new counter `palette_tile_left_hits()` (decode.rs:916, bumped at
  decode.rs:5715) counts palette-Y blocks at their own tile's left column; the gate asserts
  the delta `>= 1` on every attempt that decoded **and** compared. All 6 attempts pass it.
- Override proof (charter item 1's real intent): every arm now asserts the sequence header's
  `enable_cdef && enable_restoration` (the base tools-on recipe arrived), and the palette arm
  additionally asserts `allow_screen_content_tools` on every frame header (its own
  `--tune-content=screen --enable-palette=1` arrived).

Mutation test on a scratch copy of decode.rs (restored after each run):
- revert **`left_ok`** alone (`c > 0`) → gate still passes: the tile-left guard is
  belt-and-braces, because `start_row` wipes the left palette bands before a tile's first row.
- revert **`left_ok` + `start_row`'s palette clear** → gate FAILS (panic at stream.rs:11465).
  So the gate does bind the r11 palette-band fix, and the load-bearing half is the
  `start_row` clear, not the `left_ok` guard. Stated rather than claimed the other way.

EVIDENCE: cargo test output | `EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib multi_tile -- --test-threads=1`; two mutations of decode.rs re-run | palette arm 6/6 exact; mutation(left_ok) passes, mutation(left_ok+start_row) fails

## Charter item 3: override-shape sweep of stream.rs
`grep -n "extend(\|\.chain("` near aomenc/ffmpeg arg vectors → 3 sites:
- stream.rs:11234 `run_multi_tile_gate` — extras after base: **correct** (last wins).
- stream.rs:9266 — appends `rate_args`/`depth_args`, no flag repeated with the base list.
- stream.rs:1323 — ffmpeg (`libaom-av1` private options), where a repeated option also takes
  the last value (`av_dict_set` overwrite).
Nothing to fix; the shape is safe under last-wins, which is the measured behaviour.

## Charter item 4: the `#[ignore]`d multi-tile inter arm
Left ignored, with its reproducer. It already carries `--max-partition-size=32
--enable-tx-size-search=0` (r11), so the charter's suggested flags are the ones it runs with;
the open defect is the seed-42 32x32 block at x[224..255] y[32..63], tile attribution still
unproven. Disposition: **deferred(a decodable single-tile control of the same content)** —
unchanged from r11, no budget spent on it this round.

## Test totals
`EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib -- --test-threads=2`:
**283 passed, 0 failed, 26 ignored** (1860.6 s), log `$HOME/.cache/tiles-suite.log`.
Includes `refusal_inventory` (3 tests) and `gate_coverage` (5 tests) green, and all four
live multi-tile arms (`intra`, `intra_10bit`, `inter_10bit`, `palette`) ok; the 8-bit
`multi_tile_inter` arm stays `ignored` with its open-defect reason in the ignore string.

EVIDENCE: $HOME/.cache/tiles-suite.log | `EC_AV1_REQUIRE_AOMENC=1 nice -n 10 cargo test -p ec-av1 --lib -j3 -- --test-threads=2` | `test result: ok. 283 passed; 0 failed; 26 ignored ... 1860.64s`

## Charter premise re-measured: main does NOT already carry this fix
The charter said lane-palette2 had fixed "no `use_filter_intra` read on palette-Y blocks"
on main since 3808cf8. **False at main df5d630**: `git show main:crates/ec-av1/src/decode.rs`
has no palette condition on EITHER `use_filter_intra` read (square site ~5367, rect site
~3903) — the branch's `&& palette_y_pending.is_none()` is not a duplicate and main still
needs it. The rect site is unreachable for palette content (it refuses
`allow_screen_content_tools` wholesale at decode.rs:3596), so one guarded site is the whole
class, as the r12 sweep said.

## r12 restart (the first r12 agent was killed mid-report)
Its commit 99f0091 was complete and the tree was clean; this round finished the suite run,
the main-duplication check above, and one cosmetic fix: the two new override asserts had a
missing `\` line continuation, so their panic messages carried 30 spaces mid-sentence
(`stream.rs:11284`, `stream.rs:11304`).

## Files changed
- `crates/ec-av1/src/decode.rs` — filter-intra/palette gating fix; `PALETTE_TILE_LEFT_HITS`.
- `crates/ec-av1/src/stream.rs` — palette arm un-ignored; per-attempt tile-left palette
  assert; sequence-header + screen-tools override proofs on every arm.
