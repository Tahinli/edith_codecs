# lane-av1rect8x16 — is `decode_block_rect`'s missing `(4, 8)` chroma arm a live refusal or a documented upstream gate?

**Base:** `afa13bcff4a988534555a4ca34b84937d0b6036d` (branch `lane-av1rect8x16`, worktree `~/.cache/wt/av1rect8x16`).
**Verdict:** **(a) UNREACHABLE — documented upstream gate.** The claim is true about the `match` and false about the decoder: a 4:2:0 8x16 luma strip is never handed to `decode_block_rect` at all, so no conformant stream can reach the refusal. A conformant 4:2:0 stream that codes 8x16/16x8 luma strips decodes to completion; measured 826 of them.

---

## 0. The question, restated

`decode_block_rect` (`decode.rs:14475`) ends its non-skip chroma read with

```rust
let (chroma_set, chroma_scan): (TxbSet, &[u16]) = match (chroma_w, chroma_h) {   // decode.rs:14788
    (32, 16) => (TxbSet::ChromaRect32x16, &SCAN_32X16),
    (16, 32) => (TxbSet::ChromaRect32x16, &SCAN_16X32),
    (16, 8)  => (TxbSet::ChromaRect16x8,  &SCAN_16X8),
    (8, 16)  => (TxbSet::ChromaRect16x8,  &SCAN_8X16),
    (16, 16) => (TxbSet::Chroma16, default_scan(16)),
    (8, 32)  => (TxbSet::Chroma16, &SCAN_8X32),
    _ => {
        return Err(unsupported(                                                    // decode.rs:14804
            "a rectangular chroma transform whose size has no coefficient table",
        ));
    }
};
```

`(chroma_w, chroma_h) == (bw >> ss_x(fctx), bh >> ss_y(fctx))` (`decode.rs:14680`). A 4:2:0 8x16 luma strip halves both axes to a 4x8 chroma block, `(4, 8)` has no arm, so the string would fire **if that shape reached the function**. The sibling test `every_rect_strip_shape_the_split_path_codes_has_a_luma_and_chroma_table` audits `decode_rect_split`'s tables, not this one, so it never asks.

**State on arrival:** the string was in `REFUSALS` (`refusal_inventory.rs:50`) with **no `PROVEN` row** — the one unproven row this audit round was cataloguing.

---

## 1. CALLER SET, mechanically

`decode_block_rect(` has **9 textual matches in the crate: 1 declaration + 8 call sites.** All 8 call sites are in one function, `read_sb128_root` (`decode.rs:32852`), and none is inside a `macro_rules!` body.

The dispatch chain that reaches them:

```
SB match PARTITION_SPLIT                                   decode.rs:33651
  -> 4 x 32x32 quadrant  (r32, c32) = (sb_r*2 + q/2, sb_c*2 + q%2)   decode.rs:33652-33656
  -> part32 = dec.symbol(&mut cdfs.partition_w32[ctx32])  (BLOCK_32X32, bsize=9)  decode.rs:33663-33674
  -> match part32 {                                        decode.rs:33743
       PARTITION_HORZ   | HORZ_A | HORZ_B | VERT | VERT_A | VERT_B  ==> decode_block_rect
       PARTITION_SPLIT => 4 x 16x16 (sr, sc)              decode.rs:33774-33778
             part16 = dec.symbol(&mut cdfs.partition_w16[ctx16]) (BLOCK_16X16, bsize=6)  decode.rs:33801
             match part16 { ... }                          decode.rs:33844 / 33989 / 34037
```

| # | call site | enclosing arm | upstream guard (verbatim) | `bw`/`bh` passed | can it be handed an 8x16 / 16x8 strip? |
|---|---|---|---|---|---|
| 1 | `decode.rs:34485` | `PARTITION_HORZ =>` | `match part32 {` at `decode.rs:33743`, `part32` read at `decode.rs:33674` from `cdfs.partition_w32[ctx32]` | `at32, 32, 16` | **No** — 32-level HORZ, two 32x16 strips |
| 2 | `decode.rs:34509` | same arm, frame-edge half | `if has_rows32 {` at `decode.rs:34508`; `has_rows32` from `has_half(r32 * BLOCK_MI, BLOCK_MI, mi_rows)` at `decode.rs:33661` | `(at32.0 + 1, at32.1), 32, 16` | **No** |
| 3 | `decode.rs:34531` | `PARTITION_VERT =>` | same `match part32` | `at32, 16, 32` | **No** |
| 4 | `decode.rs:34555` | same arm, frame-edge half | `if has_cols32 {` at `decode.rs:34554`; `has_cols32` from `has_half(c32 * BLOCK_MI, BLOCK_MI, mi_cols)` at `decode.rs:33660` | `(at32.0, at32.1 + 1), 16, 32` | **No** |
| 5 | `decode.rs:34634` | `PARTITION_HORZ_A =>` | `match part32` (`PARTITION_HORZ_A` is value 8, read from the same `partition_w32` symbol) | `(at32.0 + 1, at32.1), 32, 16` | **No** — the 32x16 is the third child of a 32x32 |
| 6 | `decode.rs:34658` | `PARTITION_HORZ_B =>` | `match part32` | `at32, 32, 16` | **No** |
| 7 | `decode.rs:34790` | `PARTITION_VERT_A =>` | `match part32` | `(at32.0, at32.1 + 1), 16, 32` | **No** |
| 8 | `decode.rs:34815` | `PARTITION_VERT_B =>` | `match part32` | `at32, 16, 32` | **No** |

**The caller shape set is exactly `{(32, 16), (16, 32)}`, and every `bw`/`bh` is an integer literal — there is no variable, no computed size, no dispatch that could yield 8x16.** This is now asserted in code, not merely observed: `every_chroma_unit_decode_block_rect_can_present_has_a_coefficient_table` parses the argument lists out of `decode.rs` itself (§5).

### Where an 8x16 / 16x8 luma strip ACTUALLY goes

Every 16-level 2:1 luma strip is read by **`decode_leaf_rect`** (`decode.rs:15064`), which is a different function with the matching chroma rows:

| 16-level route | call site | strip | reader |
|---|---|---|---|
| `part16 == PARTITION_HORZ` | `decode.rs:33995` / `34014` | `16, 8` x2 | `decode_leaf_rect` |
| `part16 == PARTITION_VERT` | `decode.rs:34044` / `34063` | `8, 16` x2 | `decode_leaf_rect` |
| `part16 ∈ HORZ_A..=VERT_B` via `ab_strip!` | `decode.rs:33942`, `33946`, `33963`, `33967` | `16, 8` / `8, 16` | `decode_leaf_rect` |
| frame-edge 8-mod-16 strip | `decode.rs:34336` | `(16, 8)` or `(8, 16)` per `if has_cols16 { (16, 8) } else { (8, 16) }` (`decode.rs:34334`) | `decode_leaf_rect` |

`decode_leaf_rect` handles the 4:2:0 chroma half of exactly those strips — `(4, 8)` and `(8, 4)` — with `SCAN_4X8` / `SCAN_8X4` and `TxbSet::ChromaRect8x4` (`decode.rs:15409-15420`):

```rust
} else if bw == 16 {
    (&SCAN_16X8, &SCAN_8X4)
} else {
    (&SCAN_8X16, &SCAN_4X8)
};
```

So the `(4, 8)` arm is missing from a function that cannot be handed a 4x8, and present in the function that can.

### Chroma-domain walk over the caller set

For each caller shape, under each subsampling an AV1 `color_config` can carry:

| caller | (ss_x, ss_y) | chroma `(bw>>ss_x, bh>>ss_y)` | arm? |
|---|---|---|---|
| 32x16 | (0,0) 4:4:4 | (32, 16) | yes |
| 32x16 | (1,0) 4:2:2 | (16, 16) | yes |
| 32x16 | (1,1) 4:2:0 | (16, 8)  | yes |
| 16x32 | (0,0) 4:4:4 | (16, 32) | yes |
| 16x32 | (1,0) 4:2:2 | (8, 32)  | yes |
| 16x32 | (1,1) 4:2:0 | (8, 16)  | yes |

`(0, 1)` is **structurally uncodable**: `ec-av1-syntax`'s `color_config` only reads `subsampling_y` when `subsampling_x == 1` (`crates/ec-av1-syntax/src/sequence.rs:480-485`), so that pair never exists. `(1, 0)` is 4:2:2, refused at the sequence header by `a_non_420_subsampled_sequence_header_is_refused_by_name` — walked anyway, so the proof does not lean on that guard. **The table is complete over the caller set. The refusal is dead code.**

---

## 2. EMPIRICAL — the decisive test

Non-vacuity precondition: `HORZ_VERT_INTRA_HITS` fires at exactly three sites, `decode.rs:33990`, `34038`, `34330` — all three are `decode_leaf_rect` 16-level 2:1 strip routes. **A nonzero `horz_vert_intra_hits()` delta is therefore proof that the stream's parsed leaf list really contains a coded 16x8/8x16 luma strip**, not a proxy for it.

Sweep: 7 sources (mandelbrot 64x64 / 128x128 / 130x122, testsrc2 64x64 / 128x128, rgbtestsrc, smptebars) x cq {16,22,28,34,42,52} x (`--cpu-used`, `--max-partition-size`) ∈ {(0,16),(0,32),(1,16),(3,32),(4,64)} x `--enable-ab-partitions` {0,1} x `--reduced-tx-type-set` {0,1}, always `--codec=av1 --passes=1 --end-usage=q --threads=1 --row-mt=0 --sb-size=64 --kf-max-dist=0 --enable-rect-partitions=1 --enable-1to4-partitions=0 --enable-cdef=0 --enable-restoration=0 --min-partition-size=8 --obu`, single yuv420p key frame from `ffmpeg -f lavfi`.

```
SUMMARY attempts=840 decoded=840 streams_with_16lvl_strips=826 refusals_with_16lvl_strips=0
```

| recipe family | parsed 16-level 2:1 luma strips | decode | `"a rectangular chroma transform…"` |
|---|---|---|---|
| 840 encodes (full grid above) | 826 of 840 streams carry ≥1 (3–20 each; cq 52 cpu=0 max=16 reached 20) | **840/840 `Ok`**, 1 frame each | **0 occurrences** |
| the 14 that carry none | 0 | 840 group total, all `Ok` | 0 |

Three witness streams, each independently carrying coded 8x16/16x8 luma strips and each decoding `Ok`:

| file | recipe | 16-lvl strips | sha256 | size |
|---|---|---|---|---|
| `w_cq34_cpu0_max16.obu` | mandelbrot 130x122, `--cq-level=34 --cpu-used=0 --min-partition-size=8 --max-partition-size=16 --enable-ab-partitions=1` | 17 | `6189103938308fac94fb027448f64d0cf3fc791968cf8aae4cb27a8d904f7ae8` | 1588 |
| `w_cq28_cpu1_max16.obu` | same, `--cq-level=28 --cpu-used=1` | 13 | `e32a7f0de4d6d339716d0410f304ba985e210111a37c80c5eabf36b2301839d9` | 1991 |
| `w_cq42_cpu0_max16.obu` | same, `--cq-level=42 --cpu-used=0` | 15 | `8fee40fdfa04b0a4904e9947343c494fbb9d6958827eefb169c9e1d1e87eb629` | 1063 |

(Written to `~/.cache/av1rect8x16/`. **Not committed as a fixture**: outcome (a) needs no pin, and a fixture whose only job is to keep an unreachable guard unreachable is what the enumeration test is for.)

**A refusal on a stream whose parsed leaves carry an 8x16 rect block would be the defect. That never happened — 0 refusals over 826 non-vacuous streams.**

---

## 3. VERDICT

> **(a) UNREACHABLE.** The upstream gate is structural, not a size heuristic: all 8 call sites of `decode_block_rect` are 32-level `match part32` arms of `read_sb128_root` and pass `bw`/`bh` as the integer literals `32, 16` or `16, 32`, so the function's chroma footprint set is `{(16,8), (8,16), (16,16), (8,32), (32,16), (16,32)}` — all six of which have a row. A 4:2:0 8x16 luma strip's 4x8 chroma is read by `decode_leaf_rect`, not here, and that function carries the `(4, 8)` / `(8, 4)` rows itself. Measured over 840 real aomenc 4:2:0 key-frame encodes — 826 of which provably code a 16-level 2:1 luma strip — the refusal fires **0** times and every stream decodes to completion.

**This is NOT a decoder capability defect. No conformant stream is refused by name.**

### What the arm would have been (recorded, not added)

For the record, had an 8x16 caller appeared, libaom *does* have the shape — so the row would be legitimate, not a fabrication:

- `av1_ss_size_lookup[BLOCK_8X16][subsampling_x=1][subsampling_y=1] = BLOCK_4X8` — `~/.cache/aom-oracle/src/av1/common/common_data.c:24` (row 5 of the table; and `[BLOCK_16X8][1][1] = BLOCK_8X4` at `common_data.c:25`)
- `max_txsize_rect_lookup[BLOCK_4X8] = TX_4X8` — `~/.cache/aom-oracle/src/av1/common/common_data.h:130`
- chained by `av1_get_max_uv_txsize(bsize, subsampling_x, subsampling_y)` — `~/.cache/aom-oracle/src/av1/common/blockd.h:1372-1379`, which is `av1_get_tx_size`'s chroma branch (`blockd.h:1381-1388`)

So the 4:2:0 chroma of an 8x16 luma block is a genuine single `TX_4X8` in libaom. `decode_leaf_rect` already decodes it (`SCAN_4X8` + `TxbSet::ChromaRect8x4`, `decode.rs:15412`/`:15419`). The `(4, 8)` row in `decode_block_rect` would be a duplicate of that path for a shape no caller produces.

---

## 4. Deliverable: the claim is now PROVEN, not assumed

`refusal_inventory.rs` gained a `PROVEN` row for the string (it previously had **none** — it was the audit round's single unproven row) and the test that proves it:

- **`every_chroma_unit_decode_block_rect_can_present_has_a_coefficient_table`** (`refusal_inventory.rs:1571`)
  1. reads the caller shape set out of `decode.rs`'s own call-site argument lists (paren-balanced argument splitter, so the `(at32.0 + 1, at32.1)` origin argument does not shift the indices), asserts the site count and the shape set;
  2. reads the `match (chroma_w, chroma_h)` arm heads out of the function's own text and pins the exact six-row set (class `table-and-reader-move-together`: a table edit invalidates the test);
  3. walks every caller shape under `{(0,0), (1,0), (1,1)}` and asserts a row exists for each;
  4. asserts the residual as code, not a comment: `(4, 8)` / `(8, 4)` are *not* rows, and no caller shape can produce them.

The inventory summary moved from **32 proven to 33 proven**; all 16 `refusal_inventory` tests pass.

### Non-vacuity: three red-proofs, then revert

| mutation | result |
|---|---|
| `decode.rs:34490` `32,` → `8,` (an 8x16 caller appears) | **RED** — `the set of strip sizes decode_block_rect is called with changed … An 8x16/16x8 caller makes this refusal LIVE` |
| a `(4, 8) => (TxbSet::ChromaRect8x4, &SCAN_4X8),` row added to the table | **RED** — `decode_block_rect's chroma table changed -- re-derive the walk below` |
| `decode.rs:34490` `32,` → `at32.0 * 2,` (a call site stops being a literal) | **RED** — `decode_block_rect is called with a non-literal strip size "at32.0 * 2"` |

`decode.rs` restored byte-identical after each (`git status` shows `refusal_inventory.rs` as the only modified file).

---

## 5. Scope and hygiene

- No existing test weakened, renamed, or re-pinned. Net diff: `crates/ec-av1/src/refusal_inventory.rs`, **+240 / -0**.
- `decode_rect_split` and `read_block_tx_size` untouched (other lanes own them). `decode.rs` untouched.
- `decode_block_rect` itself untouched — the refusal stays as a named shape guard for the next caller, the repo's convention for a proven-unreachable guard (same as `a_sb_level_horz_vert_strip_admits_no_filter_intra_symbol`).
- The scratch sweep lived in two `#[ignore]`d tests, ran, and were deleted; the tree diff is the 240-line addition above.
- Main untouched: `git -C /home/tahinli/Documents/Code/Rust/edith_codecs status --porcelain` shows no tracked-file modification from this lane. Never pushed.


## 6. Checked and CLEARED (recorded so it is not re-opened)

`decode_leaf_rect` reads a 4:2:0 8x16 strip's 4x8 chroma with `TxbSet::ChromaRect8x4` + `SCAN_4X8`, while libaom's `get_txsize_entropy_ctx(TX_4X8)` reduces to the **8x8 square** set — a name that looks like a possible disagreement. It is not one: `Cdfs::txb` resolves `TxbSet::ChromaRect8x4` (`cdf_state.rs:2116`) to the *same* 8x8 chroma CDFs as `TxbSet::Chroma8` (`cdf_state.rs:1852`) — `txb_skip_chroma_8`, `eob_extra_chroma_8`, `base_chroma_8`, `base_eob_chroma_8`, `br_chroma_8`, `dc_sign_chroma`, `tx_type: None`, `side: 8` — differing only in the end-of-block group (`eob_pt_32_chroma` / `eob_pt_32_chroma_class1`, the rect-appropriate EOB_PT_32, versus `eob_pt_64_chroma`). The entropy set is the square-up one, as libaom has it; the variant name is a convenience for the distinct EOB table.
