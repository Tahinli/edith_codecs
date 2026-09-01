# lane-palette2 r10 — GREEN: merged main 06d856d, root-caused the U-plane palette defect it exposed

VERDICT: GREEN. The chartered failing cell decodes pixel-exact; both palette gates,
split_transform, tx_select, refusal_inventory and gate_coverage pass on the merged tree.

## STEP 1 — merge main 06d856d (commit 29bbbcd)
Two conflicts, resolved exactly as chartered:
- `crates/ec-av1/src/decode.rs` `reconstruct_rect` (~5892): kept this lane's `PALETTE_PRED`
  override wrapper; main's `EC_DEBUG_EDGES` eprintln moved inside the `else`, after
  `edges_rect` (it needs `above`/`left`, which only exist there).
- `crates/ec-av1/src/refusal_inventory.rs` (~50): main's list, minus
  `"a HORZ/VERT intra strip in a screen-content frame ..."` (this lane lifted it; auto-merge had
  already dropped `"a palette block with a split luma transform (round 1)"`), plus this lane's two
  palette strings; main's `"a superblock-level 1:4 partition ..."` kept, the lane's stale
  `"other than NONE or SPLIT"` copy dropped.
`cargo check -p ec-av1` clean.

## STEP 2 — the defect (class [[refusal-hides-a-defect]]), root-caused and fixed
Reproduced on the merged tree exactly as chartered (aomenc `--cq-level=45 --cpu-used=4
--sb-size=64 --min-partition-size=16 --max-partition-size=64 --enable-palette=1`,
smptebars 192x192, sha256 `d1baf950...`): **U 512 bytes wrong, x48..55 y0..63 chroma
(max |d| 28), Y and V exact.**

The charter's suspect (a stale `PALETTE_PRED` thread-local) was DISPROVED first:
`EC_DEBUG_PAL=1` counted 32 arms and 32 consumptions, `stale=false` on every arm.

Real root cause, `crates/ec-av1/src/decode.rs:4486` — `decode_block_rect`'s split-transform arm
(`depth != 0`) calls `decode_rect_split` and then `return Ok(())`, **before** the fn's tail that
stamps `record_palette_y_rect` / `record_palette_uv_rect`. So a split-tx rect strip left the
above/left palette arrays holding a DEAD block's size+colours. Traced live: the 32x16 strip at
luma (64,0) never recorded, so the next block (32x32 at luma (96,0), chroma 16x16 at (48,0))
read `palette_uv_cache = [44, 156]` — the palette of the block at luma (48,0), three blocks back —
where libaom's cache starts at 72. Cache LENGTH matched, so the same `literal(1)` hit bits and the
same 8-bit literal were read: **the entropy stream stayed in sync and only the pixels of that
palette block were wrong** (colour 0 = 44 instead of 72, hence exactly one 8x64 chroma column, and
its vertical propagation down the column). V is exact because V colours are never cached.

FIX (decode.rs:4482..4497): stamp the same two palette records at the early return.
Sweep for the same shape: `4486` is the ONLY `return Ok(())` in any `decode_block*` /
`decode_leaf*` fn (`grep -n 'return Ok(())' decode.rs` over 3800..7100 → one hit), so this is the
whole class here.

Instrument kept (charter's set/take symmetry ask): all 20 `PALETTE_PRED` set sites and both take
sites now go through `set_palette_pred` / `take_palette_pred` (decode.rs ~1052), which under
`EC_DEBUG_PAL=1` print every arm (with `#[track_caller]` location and a `stale=` flag showing an
unconsumed predecessor) and every consumption. That is what disproved the stale-thread-local
hypothesis in one run; ad-hoc probes were removed.

## STEP 3 — gates (EC_AV1_REQUIRE_AOMENC=1, --test-threads=1)
```
cd /home/tahinli/Documents/Code/Rust/edith_codecs-palette2
export CARGO_TARGET_DIR=$HOME/.cache/cargo-target-palette2 EC_NOMEMGUARD=1 EC_AV1_REQUIRE_AOMENC=1
cargo test -p ec-av1 --lib -j3 -- --test-threads=1 \
  a_real_aomenc_stream_with_rect_palette_decodes_pixel_exact \
  a_real_aomenc_stream_with_rect_screen_content_decodes_pixel_exact \
  refusal_inventory gate_coverage split_transform tx_select
```
→ **14 passed, 0 failed, 1 ignored** (133.29s). The ignored one is main's own
`a_real_aomenc_stream_with_a_split_transform_superblock_strip_decodes_pixel_exact`
(lane-rectsplit r3 RED at seed 50, not this lane's).
- `a_real_aomenc_stream_with_rect_palette_decodes_pixel_exact`: **40/70 matched pixel-exact**
  (r9: 17/70), 30 decoded-and-pixel-exact but uncounted, **0 named refusals** (r9: 32),
  `palette_rect_hits=142`.
- `a_real_aomenc_stream_with_rect_screen_content_decodes_pixel_exact`: **47/70 matched**
  (31 through a split-transform palette block), 23 uncounted-exact, 0 named refusals,
  `rect_screen_content_hits=355`, `palette_split_tx_hits=627`.
- All 70 attempts per gate now decode AND are pixel-compared; 0 mismatches.

## EVIDENCE
EVIDENCE: scratchpad/r10.obu (211 B, sha256 d1baf9503c5787d1005d358d36767195279c36c12255d10e25af106ee5849545) + r10.yuv vs ff.yuv | `decode_probe` dump vs `ffmpeg -f obu -i r10.obu -f rawvideo -pix_fmt yuv420p`, before and after the fix | before: U 512 px wrong (x48..55, y0..63, max |d| 28), Y+V exact; after: `cmp -l` = 0 bytes
EVIDENCE: scratchpad/pal.log (EC_DEBUG_PAL=1 trace of the same stream) | 32 PALSET / 32 PALTAKE lines | every arm `stale=false` — the charter's stale-thread-local suspect disproved, not patched around
EVIDENCE: scratchpad/pal4.log + pal5.log | per-block `palette_uv_cache` and palette-record trace | block r=0 c=6 (luma 96,0) read cache [44,156] stamped by luma (48,0); the strip at luma (64,0) has NO record line — the missing stamp
EVIDENCE: cargo test -p ec-av1 --lib -j3 (EC_AV1_REQUIRE_AOMENC=1), 6 filters | full 70-attempt sweep on both palette gates | 14 passed, 0 failed, 1 ignored, 133.29s
