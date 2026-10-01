# lane/av1unwritten — a MISSING REFUSAL, not an unwritten chroma unit

**Outcome in one line: `440_request_is_422.obu` is a 4:2:2 sequence header over a
4:2:0 tile, so its partition symbols name subsizes (8x16 among them) with NO
chroma plane block at ss (1,0) — libaom calls that `AOM_CODEC_CORRUPT_FRAME`
("Block size 8x16 invalid with this subsampling mode") and `aomdec` refuses the
file outright, and libaom's own encoder never emits such a shape; we walked those
subsizes anyway, and our output for that witness depends on unwritten plane
content. The fix mirrors libaom's check and returns a named refusal; the sentinel
census that led here is kept as the instrument.**

Branch `lane/av1unwritten`, base `main` = `8d6998d7`. No push.

> **r3 correction, on top of r2.** The r2 predicate was a four-cell hand-picked
> `match`, and it was wrong in BOTH directions. It refused `(16, 4)` at ss (1,0),
> where `av1_ss_size_lookup[BLOCK_16X4][1][0]` is `BLOCK_8X4` — a LEGAL 4:2:2
> shape, and one libaom's own encoder emits (`partition_search.c:3383-3389`
> gates `partition_rect_allowed[HORZ]` with this very table), so we were refusing
> a stream aomenc produces. And it covered only two of the eight shapes libaom
> refuses at 4:2:2. r3 replaces the `match` with a verbatim transcription of
> the table's `BLOCK_INVALID` cells (§3.1) and adds
> `chroma_plane_block_codable_matches_the_libaom_table`, which re-writes all 22
> rows x 4 subsampling pairs from `common_data.c:17-41` and checks the predicate
> cell for cell. The corpus sweep CANNOT see the false positive — no committed
> fixture carries a 16x8 block split `PARTITION_HORZ` at 4:2:2 — and that table
> test is what covers that direction.
>
> **r5, on top of r4.** Main: "a found defect is swept whole, not partly."
> r4 guarded only the plain `VERT` arm at four sites and named six more. r5
> makes every guard UNCONDITIONAL on the resolved symbol — because
> `partition_subsize_dims` is exact, passing the symbol covers `VERT`,
> `VERT_A`/`VERT_B`, `HORZ_4` and `VERT_4` in one call, and the HORZ siblings
> pass through — and adds the three inter-frame duplicates plus the 64x64 key-frame
> level that binds its symbol to `part` rather than `part64`. One intermediate
> attempt inserted `refuse_invalid_subsize(..., PARTITION_VERT_4, ...)` INSIDE a
> `PARTITION_HORZ_4 | PARTITION_VERT_4 =>` arm, which fired for HORZ_4 too and
> refused nine real 4:2:2 gates; the unconditional form is what shipped.
>
> **r4, on top of r3.** Main: "a found defect is swept whole, not partly."
> r3 named the six remaining sites instead of installing them. r4 installs four
> of them, via a helper that takes the PARTITION SYMBOL and maps it to its
> subsize dimensions (`partition_subsize_dims`, spec 6.10.3
> `get_partition_subsize`) so the check can never be applied to a parent's own
> shape -- the r2 mistake in a new place. The sites, their subsize, the cell
> they guard, and the SIBLING that must keep decoding are in §3.2. Two are still
> open and named (the inter-frame duplicate of the 64-level match, and the
> `decode_rect4_16_intrabc` exception, which is another lane's charter).
>
> **This report was rewritten once before. The first version of this lane claimed a libaom
> `TX_4X8` chroma unit for a `BLOCK_4X8` subsize and landed a port of it. The
> refutation pass (Yavuz-5, verified first-hand by Main) showed that port does
> not exist: `get_plane_block_size` is a single indexed return
> (`av1_ss_size_lookup[bsize][ss_x][ss_y]`, `blockd.h:1186-1193`), BLOCK_4X8 at
> ss (1,0) is `BLOCK_INVALID`, and libaom never reaches
> `max_txsize_rect_lookup` for that shape because it refuses the frame first.
> The `TX_4X8` arm, its chroma-extent derivation and its gate are all GONE; what
> follows is the corrected lane. The corrected findings are in §1–§2 and the
> correction itself is recorded here rather than quietly edited in.

---

## 1. The oracle decides this, not our reasoning

```
$ ~/.cache/aom-oracle/build/aomdec --rawvideo -o /dev/null \
      crates/ec-av1/fixtures/440_request_is_422.obu
Warning: Failed to decode frame 1: Corrupt frame detected
Warning: Additional information: Block size 8x16 invalid with this subsampling mode
```

We decoded it. That is the defect.

`av1/decoder/decodeframe.c:1449-1458`, in `decode_partition`, before it descends
into any subsize:

```c
const struct macroblockd_plane *const pd_u = &xd->plane[1];
if (get_plane_block_size(subsize, pd_u->subsampling_x, pd_u->subsampling_y) ==
    BLOCK_INVALID) {
  xd->mi_row = mi_row;
  aom_internal_error(xd->error_info, AOM_CODEC_CORRUPT_FRAME,
                     "Block size %dx%d invalid with this subsampling mode",
                     block_size_wide[subsize], block_size_high[subsize]);
}
```

`get_plane_block_size` (`blockd.h:1186-1193`) is a **single indexed return**,
`av1_ss_size_lookup[bsize][subsampling_x][subsampling_y]` — no per-axis MIN. The
`BLOCK_INVALID` cells in that table (`common_data.c:20-26`) are:

| subsize | ss (0,0) 4:4:4 | ss (0,1) | ss (1,0) 4:2:2 | ss (1,1) 4:2:0 |
|---|---|---|---|---|
| 4x8   | 4x8 | 4x4 | **INVALID** | 4x4 |
| 8x4   | 8x4 | **INVALID** | 4x4 | 4x4 |
| 8x16  | 8x16 | 8x8 | **INVALID** | 4x8 |
| 16x4  | 16x4 | **INVALID** | 8x8 | 4x4 |

So at 4:2:2 a **4x8, 8x16 or 16x4** subsize is not decodable at all. It is not a
"the chroma unit is 4x8" case: libaom never computes a transform size for it.
(The earlier `8x16` in the oracle's message is the first such subsize the
witness's 4:2:0 tile presents; 4x8 and 16x4 are in the same class.)

`PARTITION_HORZ` on an 8x8 is `subsize_lookup[HORZ][BLOCK_8X8] = BLOCK_8X4`,
whose 4:2:2 plane block is BLOCK_4X4 — the square unit, which is why the
`(4, 8)` chroma path itself was never wrong. `BLOCK_4X8` leaves come from
`PARTITION_VERT`, i.e. from `decode_leaf_rect8(vert = true)`, and that subsize is
exactly one of the four libaom refuses.

---

## 2. What led here, and what it is worth

### 2.1 The observation (on `8d6998d7`, by two independent builds)

```
$ mkdir -p /tmp/wt_aw/same          # the dump writes NOTHING if the directory
                                    # does not exist -- create it first
$ for i in 1 2 3 4 5; do EC_AV1_FINAL_DUMP=/tmp/wt_aw/same/x \
    ./target/debug/examples/decode_probe crates/ec-av1/fixtures/440_request_is_422.obu \
    >/dev/null 2>&1; sha256sum /tmp/wt_aw/same/x.f0; done
727ea647b2362b22520de232c2f6a0ce541dbfa90f02495f572af4aa2297116d
ba2957e1709507b8336e53cd908b7dfe2a59dc2064b1f5eb4bea9b3149a4653d
2e245bcc6abb2ef81e80254711fe042cadd3409c622faa46b9ee393dd14ffa96
```

Two distinct output hashes from one file with uninitialised planes; and with
`EC_AV1_PLANE_SENTINEL=1` five runs give ONE hash, which differs from the plain
one (1984 bytes). Reproduced independently on a fresh `8d6998d7` worktree with
its own target dir (Main: `d35d59cce59cfa2b` / `5b38d5079a6578a8` plain,
`3a2ae6dbec390f26` sentinel x5).

**Provenance note, because it nearly inverted this section.** The first
counter-measurement said main was deterministic; that reading came from a binary
whose embedded source paths pointed at THIS worktree, not at main -- the
stale-binary class. Provenance has to come from the binary's own strings, not
from what `cargo build` last reported. With a path-verified build the
nondeterminism is real, and this is the observation that started the lane.

**Byte width matters for the census.** `EC_AV1_FINAL_DUMP` writes **u8** for an
8-bit stream, so `PLANE_SENTINEL = 0xDEAD` narrows to the single byte `0xAD`:
65 such bytes in a sentinel dump against a 33 baseline in a plain one. Counting
a two-byte `0xDEAD` in an 8-bit dump cannot find it.

### 2.2 The census, on this lane's tree

`census_unwritten` at each frame's pre-deblock point, on the lane tree at r1:
**112 unwritten samples per chroma plane, 224 total**, in whole 4x4 chroma
blocks, each the lower half of a plane block whose luma subsize is one §1 says
is not decodable. That number is a lane-tree measurement with a lane-only
instrument and is cited as such; the fix does not rest on it.

### 2.3 Why refusing is the right contract

Neither of these is this lane's pixel measurement:

1. **libaom refuses the shape.** `aomdec --rawvideo` on the pin prints
   `Failed to decode frame 1: Corrupt frame detected` / `Block size 8x16 invalid
   with this subsampling mode` (`decodeframe.c:1449-1458`).
2. **libaom's encoder never emits it.** `partition_search.c:3383-3389` gates
   `partition_rect_allowed` on this same table, so no conformant 4:2:2 stream
   can contain the shape and the refusal cannot reject legal content.

§2.1 explains why anyone looked; §2.3 is why the fix is right.

The instrument is **kept**: env-gated, off by default, and the only way to ask
"did the tile walk write every sample it was handed" (a wrong-sample diff cannot
tell an unwritten sample from a mis-predicted one).

## 3. The fix

`crates/ec-av1/src/decode.rs`:

* `chroma_plane_block_codable(bw, bh, ss_x, ss_y)` — a verbatim transcription
  of `av1_ss_size_lookup`'s `BLOCK_INVALID` cells, in two const lists so a
  reader can diff them against `common_data.c` line by line:

  | subsampling | INVALID luma shapes |
  |---|---|
  | 4:2:2 `(1,0)` | 4x8, 8x16, 16x32, 32x64, 64x128, 4x16, 8x32, 16x64 |
  | 4:4:0 `(0,1)` | 8x4, 16x8, 32x16, 64x32, 128x64, 16x4, 32x8, 64x16 |
  | 4:4:4 `(0,0)`, 4:2:0 `(1,1)` | none |

  Checked by `chroma_plane_block_codable_matches_the_libaom_table`, which
  transcribes all 22 rows x 4 pairs literally from `common_data.c:17-41`.
* `refuse_invalid_plane_block(...)` — a **returned** error (never an
  `assert!`: a partition symbol is stream data, so hostile bytes reach it),
  carrying libaom's own wording so the message names the rule:
  `"... has no chroma plane block at this frame's subsampling mode (libaom:
  \"Block size %dx%d invalid with this subsampling mode\",
  av1/decoder/decodeframe.c:1456, refusing by the same rule)"`.
* Installed at every place a partition symbol is turned into a **strip** subsize
  with a chroma plane block:
  * `partition_w8` → `PARTITION_HORZ` = 8x4, `PARTITION_VERT` = 4x8
    (`subsize_lookup[PARTITION_*][BLOCK_8X8]`);
  * `partition_w16` → `PARTITION_HORZ` = 16x8, `PARTITION_VERT` = 8x16.

```
$ ./target/debug/examples/decode_probe crates/ec-av1/fixtures/440_request_is_422.obu 2>&1 | head -4
TILING: 1 frame headers parsed
SEQ: use_128x128_superblock=true bit_depth=8 mono_chrome=false max_frame=64x64
TILING: cols=1 rows=1 uniform_spacing=true context_update_tile_id=0
REFUSED: unsupported: AV1 tile (a block size 4x8, 8x16 or 16x4 (or 8x4 at 4:4:0) has no
chroma plane block at this frame's subsampling mode (libaom: "Block size %dx%d invalid with this
subsampling mode", av1/decoder/decodeframe.c:1456, refusing by the same rule))
```

Same refusal class as the oracle, same rule, same shape named.

### 3.3 The r5 site table

Every site below is guarded UNCONDITIONALLY: the symbol is passed to
`refuse_invalid_subsize`, and because `partition_subsize_dims` is exact, every
arm of the dispatch is covered at once — `VERT`, `VERT_A`/`VERT_B`, `HORZ_4` and
`VERT_4` — while the HORZ siblings pass through. That replaced r4's narrower
`matches!(sym, PARTITION_VERT)` guards, which covered only the plain `VERT` arm.

| site (key frame unless noted) | subsize it can select | guarded cell | sibling that must keep decoding |
|---|---|---|---|
| 128 root, `match part128` | `VERT`/`VERT_A`/`VERT_B` -> 64x128 | `(1,0)` INVALID | `HORZ`/`HORZ_A`/`HORZ_B` -> 128x64, `(1,0)` = 64x64 valid |
| 64 level, `match part` (symbol bound to `part`) | `VERT`/`VERT_A`/`VERT_B` -> 32x64; `VERT_4` -> 16x64 | `(1,0)` INVALID | `HORZ*` -> 64x32 `(1,0)` = 32x32 valid; `HORZ_4` -> 64x16 `(1,0)` = 32x16 valid |
| 32 level, `match part32` | `VERT*` -> 16x32; `VERT_4` -> 8x32 | `(1,0)` INVALID | `HORZ*` -> 32x16 `(1,0)` = 16x16 valid; `HORZ_4` -> 32x8 `(1,0)` = 16x8 valid |
| 16x16 level, `match part16` | `VERT_4` -> 4x16 | `(1,0)` INVALID | `HORZ_4` -> 16x4, `(1,0)` = **8x4 valid** — the shape r2 refused |
| 8 level, `partition_w8` arms (r3) | `VERT` -> 4x8 | `(1,0)` INVALID | `HORZ` -> 8x4, `(1,0)` = 4x4 valid |
| **inter** 128 root, `match part128` | 64x128 | `(1,0)` INVALID | as key |
| **inter** 64 level, `match part64` | 32x64; `VERT_4` -> 16x64 | `(1,0)` INVALID | as key |
| **inter** 32 level, `match part32` | 16x32; `VERT_4` -> 8x32 | `(1,0)` INVALID | as key |
| **inter** 16x16 level, `match part16` | 4x16 | `(1,0)` INVALID | `HORZ_4` -> 16x4 valid |

`decode_rect4_16_intrabc` remains the declared exception (another lane's
charter). The 8x16 / 16x8 `VERT_4` case is covered by the level-16 guard, whose
symbol is the same `PARTITION_VERT_4`; the shape it selects is the four-way child
`4x16` at that level, not the `16x4` r2 wrongly refused.

**Still open:** none of the four items in r5's list. Each is a one-line call
behind the existing helper and all four landed.

### 3.2 The r4 site table

| site | subsize it can select | cell it guards | sibling that must keep decoding |
|---|---|---|---|
| 128 root, `PARTITION_VERT` (`decode.rs`, `match part128`) | 64x128 | `(1,0)` INVALID | `PARTITION_HORZ` -> 128x64, `(1,0)` = 64x64 valid |
| 64-level, `PARTITION_VERT` (`match part64`) | 32x64 | `(1,0)` INVALID | `PARTITION_HORZ` -> 64x32, `(1,0)` = 32x32 valid |
| 32-level, `PARTITION_VERT` (`match part32`) | 16x32 | `(1,0)` INVALID | `PARTITION_HORZ` -> 32x16, `(1,0)` = 16x16 valid |
| 16x16-level, `PARTITION_VERT_4` (`match part16`) | 4x16 | `(1,0)` INVALID | `PARTITION_HORZ_4` -> 16x4, `(1,0)` = 8x4 **valid** -- the exact shape r2 refused, and the one libaom's encoder emits |
| 16-level, `PARTITION_HORZ`/`VERT` | 16x8 / 8x16 | 8x16 `(1,0)` INVALID | `PARTITION_HORZ` -> 16x8, `(1,0)` = 8x8 valid |
| 8-level, `PARTITION_HORZ`/`VERT` | 8x4 / 4x8 | 4x8 `(1,0)` INVALID | `PARTITION_HORZ` -> 8x4, `(1,0)` = 4x4 valid |

The sibling column is the r2 lesson turned into a rule: at every site the
opposite-axis branch of the SAME dispatch must keep decoding, and the table test
plus these guards are what prove it. `decode_rect4_16_intrabc` is the declared
exception (another lane's charter).

### 3.1 Sites found, and the ones proven not to need it

| site | subsizes it can produce | needs the check? |
|---|---|---|
| `partition_w8` (2 sites, 36549 / 36767 region) | 8x4, 4x8 | **yes**, installed |
| `partition_w16` intra (36175 region) | 16x8, 8x16 | **yes**, installed |
| `partition_w16` edge split (`VERT_ALIKE`/`HORZ_ALIKE` gather) | same two, reached only when `has_cols16`/`has_rows16` is false | covered by the same rule at the sibling site; the edge arm's two-symbol gather can only choose SPLIT vs the matching strip |
| `PARTITION_VERT_4` at the 16x16 level | 4x16 — INVALID at 4:2:2 | **yes, r4, installed** |
| `partition_w32` `PARTITION_VERT` | 16x32 — INVALID at 4:2:2 | **yes, r4, installed** |
| `partition_w64` `PARTITION_VERT` | 32x64 — INVALID at 4:2:2 | **yes, r4, installed** |
| 128 root `PARTITION_VERT` | 64x128 — INVALID at 4:2:2 | **yes, r4, installed** |
| the four-way vertical splits (`VERT_A` / `VERT_B`) at 16/32/64 | 8x32, 16x64 — both INVALID at 4:2:2 | **still open, named** |
| `VERT_4` on 8x16 / 16x8 (`decode_rect_split`) | 8x32 — INVALID at 4:2:2 | **still open, named** |
| the inter-frame duplicate of the 64-level match | 32x64 | **still open, named** |
| `decode_rect4_16_intrabc` | 4x16 / 8x32 | **out of scope** — another lane's charter |
| 32/64/128 roots' OWN plane blocks, the AB partitions, `PARTITION_SPLIT` | BLOCK_32X32 (16X32), BLOCK_64X64 (32X32), BLOCK_128X128 (64X64), all valid | no — a PARENT row being valid says nothing about its children, which is exactly what r2 got wrong; the child rows are the four rows above |
| 4:4:4 square leaves, `decode_leaf8`, `decode_leaf_split4` | BLOCK_4X4 / BLOCK_8X8 / BLOCK_16X16 | no: `(0,0)` column is non-INVALID for all of them |

So r3 installs the check at two sites and NAMES the other six rather than
claiming them covered. The predicate is now correct for every shape, so each
remaining site is a one-line call at a dispatch that is already written; none of
them is reachable by a committed fixture (§4 shows the corpus cannot see even
the false-positive direction), which is why they are a declared gap and not a
claim.

The 1:4 sites were already the honest gap: `PARTITION_HORZ_4` on an 8x16 yields a 4x16,
whose 4:2:2 plane block is `BLOCK_INVALID`, and this lane did not install the
check there. `decode_rect4_16_intrabc` is on another lane's charter and was not
touched; the neighbouring non-intrabc `decode_rect4_16_strip` site is where the
next step belongs. No committed fixture reaches it (see §4), and the census
instrument is what would show it if one ever did.

---

## 4. Regression evidence

* **Corpus, both trees.** Every fixture decoded with `EC_AV1_FINAL_DUMP`, every
  `.f*` hashed. Pre-fix vs post-fix the **126 valid fixtures are byte-identical**
  and **none newly refuses**; the witness is the only difference and it now
  refuses. Re-run on the r5 predicate: `differing:
  ['440_request_is_422']`, 126 fixtures emitting dumps, and exactly one fixture
  whose output contains `REFUSED` — the witness. **The sweep cannot see the
  (16,4)-at-4:2:2 false positive**: no committed fixture carries a 16x8 block
  split `PARTITION_HORZ` at 4:2:2, which is why r2's over-refusal passed 126/126
  unnoticed. That direction is covered by
  `chroma_plane_block_codable_matches_the_libaom_table`, not by the sweep.
* **Sentinel sweep** (127 fixtures, `EC_AV1_PLANE_SENTINEL=1`): pre-fix exactly
  one fixture had a non-empty census (the witness, 112+112); post-fix the census
  is empty on all 127 — the witness decodes nothing at all now, and the other 126
  are unchanged. **This is NOT a corpus-wide claim that no fixture's output
  depends on unwritten plane memory.** The census is a PRE-DEBLOCK scan of the
  three planes, so it cannot see a dependence produced at a later stage or in a
  region it does not cover. Measured on `8d6998d7` with a provenance-verified
  probe (Main, after r5), a sentinel-vs-plain output diff finds at least two
  further fixtures whose output DOES depend on unwritten content —
  `hg_rect64_intra16x4_witness.obu` (34 frames) and `hg_arf_witness.obu` (40
  frames) — both of which this census calls clean. One of the two instruments has
  a blind spot; `Kerem-9` owns finding which (`lane/unwritten-dep`). Every number
  above is this lane's own tree and instrument.
* **Scoped tests.** `cargo test -p ec-av1 --features gate-counters --lib -- 422
  440 chroma422` → 15 passed (including six real-4:2:2 byte-exact gates:
  `a_real_422_key_frame_and_inter_sequence_decode_pixel_exact`,
  `a_real_422_12bit_and_superres_stream_decode_pixel_exact`,
  `a_real_422_film_grain_stream_decodes_pixel_exact`,
  `the_pinned_422_palette_intra_in_inter_cell_window_is_byte_exact`,
  `the_pinned_422_lossless_inter_witnesses_decode_pixel_exact`,
  `the_pinned_422_bigblock_witnesses_decode_pixel_exact`);
  `-- sub8 decode::tests` → 53 passed. The 15-test set also covers the
  pre-existing `the_440_cell_is_not_a_codable_chroma_shape`, whose tail arm
  lane-av1422lift had amended to "the pin decodes"; that arm is now the refusal
  assertion above and the rest of the gate (the 4:4:0-request-lands-on-4:2:2
  claim) is untouched.
  `-- sub8 picalloc sentinel` → 13 passed; `-- decode::tests` → 40 passed.

---

## 5. The gate

`crates/ec-av1/src/stream.rs`,
`a_422_header_over_a_420_tile_refuses_the_subsize_libaom_calls_corrupt`:

1. pins `440_request_is_422.obu` through the existing `read_pin` (2014 bytes,
   fnv1a64 `0x98a1378df976253d`);
2. asserts the decode is an **error**, and that the message contains libaom's
   own `"invalid with this subsampling mode"` — so a gate cannot pass on any
   other refusal;
3. **positive control**: `422_key_64x64.obu`, a genuine 4:2:2 pin, must still
   decode to at least one frame — the gate cannot pass by refusing everything.

```
$ cargo test -p ec-av1 --features gate-counters --lib -- refuses_the_subsize
test stream::tests::a_422_header_over_a_420_tile_refuses_the_subsize_libaom_calls_corrupt ... ok
test result: ok. 1 passed; 0 failed; ...
```

**The pin.** The witness no longer produces output at all — it refuses, which is
the contract libaom has (§2.3). §2.1 records the observation that made the
stream worth looking at (varying output with uninitialised planes, one hash
under the sentinel); it is reported, not gated, because the gate's job is the
contract and the contract is the refusal. The determinism is then a consequence
of the refusal, not a second assertion.

**Mutation.** Deleting the two `chroma_plane_block_codable` call sites puts the
witness back to `OK: 1 frames decoded` and the gate's `unwrap_or_else` panics
with "the pin must be REFUSED, not decoded"; replacing the message text so it no
longer contains libaom's wording reds the second assertion; pointing the positive
control at the witness reds the third.

---

## 6. What this lane did not do

* The `PARTITION_HORZ_4` / `VERT_4` 1:4 subsize sites (4x16 / 16x4) are not
  checked — §3.1 names them and where the next step belongs. No committed
  fixture reaches them.
* No pixel claim is made or unmade for the witness: it is not a decodable 4:2:2
  stream and `aomdec` says so.
* `film_grain.rs`, the reserved 4:2:0 group-tail chroma SKIP arm and
  `decode_rect4_16_intrabc` untouched — the cause is not in them.

---

## 7. Traps this lane paid for

* **`EC_AV1_FINAL_DUMP` writes NOTHING when the prefix's directory does not
  exist** — silence, not an error, so a run that "produced no dump" reads as a
  decode that produced no frame. Create the directory first. This cost a
  measurement round here and one for Main.
* **At 8-bit the dump is u8, so `PLANE_SENTINEL = 0xDEAD` survives only as the
  single byte `0xAD`.** Counting a two-byte `0xDEAD` in an 8-bit dump cannot find
  it (65 such bytes against a 33 baseline is what a sentinel dump actually
  carries here). Use a 0xAD count at 8-bit, or a 10-bit fixture where the dump is
  u16 LE.
* **A binary's provenance comes from its own embedded source paths**, not from
  what `cargo build` last reported. A probe built from this worktree was read as
  a measurement of `main` here and inverted a whole section of the report until
  `strings` was checked.
