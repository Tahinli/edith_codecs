# lane-av1chrtx — rect inter chroma per-unit tx_type (H1) + rect coeff-trace plane tag (H2)

Branch `lane-av1chrtx` (worktree `~/.cache/wt/av1chrtx`), base `a21f3680`.
Oracle `~/.cache/aom-oracle/build/{aomdec,aomenc}` (instrumented).
`CARGO_TARGET_DIR=$HOME/.cache/cargo-target-av1chrtx`.

Commits: `d791c464` (H2), `2142d2cf` (H1) — one per defect, as chartered.

---

## H1 — FIXED. `read_inter_rect_chroma` handed every chroma unit the BLOCK-level `tx_type`

### The defect

`read_inter_rect_chroma` is the sibling of the two 4:4:4 four-unit arms.
lane-av1444chr extracted `covering_leaf_tx_type` and re-pointed both
four-unit arms at it, but left this sibling passing the block-level
`luma_tx_type` (the top-left leaf's type) to **every** unit of the plane:

```rust
let inherit_tx_type = Some(luma_tx_type);   // before
```

`av1_get_tx_type` (`blockd.h:1287-1297`) reads `xd->tx_type_map` at the
chroma unit's own position scaled back to luma, and `read_coeffs`'s
`all_zero` arm stamps `DCT_DCT` into that leaf's map cell
(`decodetxb.c:199-203`) — so a unit sitting over a luma leaf that coded
**nothing** inherits `DctDct`, not the block-level type. With
`nx*ny > 1` over mixed var-tx leaves that is a per-unit wrong answer.

### The witness hunt (bounded, then found)

lane-av1444chr's report recorded the route as unreachable on five
4:4:4 rect recipes (`multi` = 0 on every one). Two facts explain why,
and they shape the hunt:

1. **`multi` is false by construction at 4:2:0.** The arm is only reached
   for `side <= 64` blocks (the `side > 64` mu-chunk path runs first), and
   a `side <= 64` block's 4:2:0 chroma plane is at most 32x32 — exactly
   one `uw x uh` unit, so `nx*ny == 1` always. The earlier hunt's
   recipes were 4:4:4 but used partition settings that never produced a
   block keeping a 64-px axis.
2. So the arm needs **4:4:4 AND a rect block with a 64-px luma axis**
   (32x64 / 64x32 / 64x16 / 16x64) — a 4:4:4 chroma plane then exceeds
   the 32 cap on one axis and splits.

Hunt: 200+ encodes, three 4:4:4 sources (testsrc2 128/192/256, mandelbrot
128/256, testsrc2 192x256), swept over `--min-partition-size` 8/16/32,
`--max-partition-size` 32/64/128, `--enable-tx-size-search` 0/1,
`--enable-rect-partitions`, `cq-level` 18-44, `--cpu-used` 0/2/3/5/7,
sizes 128x128 through 256x256. Instrumented with a temporary
`EC_AV1_CHRTX_PROBE` eprintln in the arm printing `(nx, ny, multi, leaves)`
(removed before the commit).

Result: the arm's `multi` is 0 on every 4:2:0 recipe and on every 4:4:4
recipe whose rect strips keep a 32-px axis. It first fires with
**`--cpu-used=0 --min-partition-size=32 --max-partition-size=64` on a
256x256 source** — the slow encoder is what actually picks a 32x64 strip
over two 32x32 blocks.

### The witness (pinned)

`crates/ec-av1/fixtures/444_rect_strip_leaf_tx_type.obu`, 26839 bytes,
sha256 `06174a66e92aaeb751a8e86db59711fcb1f18d74bde2d77e1ef1be7983eb7c80`,
fnv1a64 `0xc77f310c036dff73`.

```
ffmpeg -f lavfi -i "testsrc2=size=256x256:rate=25" -frames:v 3 -pix_fmt yuv444p \
  -f yuv4mpegpipe ts256.y4m
aomenc --codec=av1 --profile=1 --cq-level=30 --cpu-used=0 --sb-size=64 \
  --min-partition-size=32 --max-partition-size=64 --enable-tx-size-search=1 \
  --passes=1 --end-usage=q --threads=1 --row-mt=0 --lag-in-frames=0 \
  --kf-max-dist=100 --limit=3 --obu -o w.obu ts256.y4m
```

The qualifying block is the **32x64 inter strip at mi(32,48)**:
a 4:4:4 chroma plane of `(32, 64)` = **two** TX_32X32 units, over a
five-leaf luma tree

| leaf (row, col, tw, th, tx_type) | covering mi rows |
|---|---|
| `(0, 0, 32, 32, Idtx)` | 0..8 |
| `(8, 0, 16, 16, VDct)` | 8..12 |
| `(8, 4, 16, 16, DctDct)` | 8..12 |
| `(12, 0, 16, 16, HDct)` | 12..16 |
| `(12, 4, 16, 16, VDct)` | 12..16 |

The **lower** chroma unit (its own mi cell `(8, 0)`) therefore resolves
`VDct` where the block-level value is `Idtx` — once per chroma plane, so
`diff = 2`. The upper unit resolves `Idtx`, i.e. the same as the
block-level value: the defect is strictly per-unit.

### The fix

- `read_inter_rect_chroma` takes the block's `leaf_tx_types` and resolves
  `inherit_tx_type` **per unit** through the shared
  `covering_leaf_tx_type(leaf_tx_types, unit_rel_mi)`, with
  `.unwrap_or(luma_tx_type)` as the fallback for a block that coded a
  single luma unit (no vartx tree, so `leaves` is empty). Both call sites
  (the compound and single-reference copies of `decode_inter_block`)
  pass their `&leaf_tx_types` — the same list the four-unit arms read.
- Two new gate counters, `CHROMA_RECT_LEAF_TX_HITS` /
  `CHROMA_RECT_LEAF_TX_DIFF_HITS`, deliberately **separate** from
  `CHROMA_QUAD_LEAF_TX_HITS` / `_DIFF_HITS`: the latter pair gates a
  4:2:0 control asserting exactly `(0, 0)`, and this route **is**
  reachable at 4:2:0, so sharing the counter would destroy that
  control's meaning.

### Bar

- **Red-before by mutation** (revert only the resolve to the block-level
  type; everything else intact):
  `decode-order frame 1 of 3 (3 shown, 0 hidden) differs from the oracle
  at byte 105949 (ours 166 vs 167), 1998 bytes differ`.
  Restored: green. (A first mutation attempt used `.or(None::<TxType>)`,
  which is the identity on `Option` and therefore did **not** bite — the
  red run above uses `.and(None::<TxType>)`, the real removal.)
- **New gate** `stream::tests::a_pinned_444_rect_inter_stream_resolves_each_chroma_unit_from_its_own_luma_leaf`:
  fixture length + fnv pinned, `decode_all_frames_vs_oracle` (length-
  asserting, byte-exact, 3 frames, 0 hidden), asserts `hits >= 2*diff &&
  hits > 0` and **`diff >= 2`**.
  Measured: `rect chroma units resolved from their own leaf 4, of which 2 differed`.
- **In-gate control**: the committed
  `fixtures/444_sb128rect_lr_witness.obu` fires the same route (2 units
  resolved) with **0** differing types — the exact shape a no-op fix
  looks green on, which is why the DIFF counter and not the route counter
  is the non-vacuity bar.
- **Resolver unit test** `decode::tests::covering_leaf_tx_type_picks_the_leaf_under_the_unit_or_reports_none`:
  mixed-leaf lookup on the witness's own five-leaf tree, interior cells
  (containment, not raster position), the mi cell just past the block's
  extent → `None`, and the empty leaf list → `None`.
  Mutation-proven red: turning the containment `<` into `<=` →
  `panicked at crates/ec-av1/src/decode.rs:52883, left: Some(Idtx), right: Some(VDct)`.
  Restored: green.
- **Identity green** (13 tests, one binary, `--test-threads=4`, no SKIP
  lines): the new gate, the resolver unit test, the av1444chr gate
  (`quad-resolved chroma units 96, of which 8 differed` — unchanged), plus
  `a_444_lossy_rect4_inter_stream_decodes_pixel_exact`,
  `a_444_sb128_root_rect_stream_with_restoration_decodes_pixel_exact`,
  `a_lossless_444_defaultp_inter_strip_stream_decodes_byte_exact`,
  `a_lossless_444_min_partition64_inter_stream_decodes_pixel_exact`,
  `a_lossless_444_min_partition8_inter_stream_decodes_sample_exact`,
  `a_real_aomenc_rect_inter_block_predicts_chroma_with_the_narrow_kernel_pixel_exact`,
  `a_real_aomenc_rect_strip_palette_decodes_pixel_exact`,
  `a_real_aomenc_inter_sequence_with_a_16_level_rect_leaf_decodes_pixel_exact`,
  `a_444_intrabc_rect4_reads_its_own_chroma_plane_block`,
  `a_lossless_444_rect16x4_chroma_reach_is_ss_aware` — **13 passed, 0 failed**.

---

## H2 — FIXED. `read_coeffs_rect`'s `EC_COEFF_STEP` traces hardcoded `plane=0`

`read_coeffs_rect` printed `plane=0` on both tagged lines
(`tag=all_zero`, `tag=tx_type`), so every **chroma** rect coefficient
read was logged as a luma one. Two independent lane reports mis-read a
chroma unit's `txb_skip` / `tx_type` symbol on those lines and followed
the luma arm — the log could not tell them apart.

Fix: a `plane: usize` parameter on `read_coeffs_rect`, with each of the
twenty call sites passing its own plane (`0` for the luma readers,
`plane_idx` / `plane` where the enclosing reader already has one).
`read_chroma_coeffs_rect` takes the same parameter and passes it
through, so the U and V call sites in `decode_block_rect`,
`decode_leaf_rect`, `decode_block_rect4`, `decode_rect4_16_strip` and
`decode_block_rect64` stop collapsing into one tag.

**Measurement** (`EC_TRACE_COEFF=1` on
`a_444_lossy_rect4_inter_stream_decodes_pixel_exact`):

| line | before | after |
|---|---|---|
| `tag=all_zero` | 932 lines, all `plane=0` | 312 `plane=0` / 310 `plane=1` / 310 `plane=2` |
| `tag=tx_type` | 110 lines, all `plane=0` | 110 `plane=0` (this set's units are luma-only on this fixture) |

No trace output changes in a default build: both lines are behind
`coeff_trace_on()`, i.e. the env-gated `EC_TRACE_COEFF`. No gate output
changes — the counter and assert set is untouched by this commit.

---

## Not run here (Main's validation)

Full crate suite, the multi-encode sweeps
(`a_real_aomenc_inter_sequence_with_a_16_level_rect_leaf_decodes_pixel_exact`
was run as a single named test only), the 4:2:0/10-bit/12-bit sweep
families, and the VPS stages. Local runs were single named tests / one
named-test batch, per the lane's local-scope rule.
