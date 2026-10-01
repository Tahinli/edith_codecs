# lane/av1unwritten — a 4:2:2 chroma block the walk never covered

**Outcome in one line: `sub8_leaf_chroma422` wrote ONE square `TX_4X4` where libaom
writes ONE `TX_4X8` (`av1_get_max_uv_txsize(BLOCK_4X8, 1, 0)`), so every 4:2:2
8x8 block coded `PARTITION_HORZ` left the bottom four chroma rows unwritten and
the uninitialised plane memory reached the output; the fix routes that shape to
the port that already exists for a `BLOCK_8X8` leaf (`leaf8_chroma422_unit`), and
a new sentinel-census gate pins "the walk wrote every sample it was handed".**

Branch `lane/av1unwritten`, base `main` = `8d6998d7`. No push.

---

## 1. Reproduction: five runs, five hashes

```
$ cd /home/tahinli/.cache/wt/av1unwritten
$ cargo build -p ec-av1 --example decode_probe --features gate-counters
$ mkdir -p /tmp/wt_aw
$ for i in 1 2 3 4 5; do EC_AV1_FINAL_DUMP=/tmp/wt_aw/u$i \
      ./target/debug/examples/decode_probe crates/ec-av1/fixtures/440_request_is_422.obu \
      >/dev/null 2>&1; sha256sum /tmp/wt_aw/u$i.f0; done
de5051156b7cfd00dc7ed5f2c6ad404e2a49822a8a728bc027a938aac00219f1  /tmp/wt_aw/u1.f0
eb0e513ff07d9780b03e70861d1a7f507f540e4724f62384511a71e06c386aa9  /tmp/wt_aw/u2.f0
2985b82874579807efb8bc88136d1b8f5f1f0121410cde26723d5be7e6ba8715  /tmp/wt_aw/u3.f0
204645ad20d7e557d7b04f0027d9aafd5790fe685dc39bdf7cfb117d9dd1f0fc  /tmp/wt_aw/u4.f0
80908a1712c4fe78070a6a3ad57464e8bd6a9d0b8d73e86e8f9cf7ea7cbd3496  /tmp/wt_aw/u5.f0
```

Five hashes from one file. Under `EC_AV1_PLANE_SENTINEL=1` the same five runs give
ONE hash — the output difference is exactly the samples the decode never wrote.

### 1.1 The instrument (now committed)

`EC_AV1_SENTINEL_CENSUS=1` (new, `decode::census_unwritten`, called at each
frame's pre-deblock point beside the existing `EC_AV1_PREFILT_DUMP16` rung, on
both the key-frame and inter-frame paths) scans each plane over its own
`true_width` x `true_height` extent and prints, per plane, the number of samples
still holding `PLANE_SENTINEL` plus every hole as per-row column spans. This is
the shipped form of the four-line temporary scan `lanes/av1422seed.report.md` §3
used; the report explicitly noted it was *not* committed, which is why this lane
had to re-derive it.

```
$ EC_AV1_PLANE_SENTINEL=1 EC_AV1_SENTINEL_CENSUS=1 \
    ./target/debug/examples/decode_probe crates/ec-av1/fixtures/440_request_is_422.obu 2>&1 >/dev/null
SENTINEL_CENSUS idx=0 plane=Y extent=64x64 unwritten=0
SENTINEL_CENSUS idx=0 plane=U extent=32x64 unwritten=112 spans=24:
  y36 x0..=3 y36 x12..=15 y37 x0..=3 y37 x12..=15 y38 x0..=3 y38 x12..=15 y39 x0..=3 y39 x12..=15
  y44 x4..=7 y44 x12..=15 y45 x4..=7 y45 x12..=15 y46 x4..=7 y46 x12..=15 y47 x4..=7 y47 x12..=15
  y52 x0..=7 y53 x0..=7 y54 x0..=7 y55 x0..=7 y60 x0..=3 y61 x0..=3 y62 x0..=3 y63 x0..=3
SENTINEL_CENSUS idx=0 plane=V extent=32x64 unwritten=112 spans=24: (identical)
```

**112 samples per chroma plane, 0 luma.** Both chroma planes, so the census is
not a one-sided artefact.

### 1.2 The holes are whole 4x4 chroma blocks, and each is a whole block's bottom half

Grouped as blocks, the six spans are six 4x4 chroma blocks (two of the spans are
8 wide = two blocks):

| chroma block (x, y) | luma extent (2x, y..y+3) | owning luma group |
|---|---|---|
| (0..3, 36..39) | (0..7, 36..39) | 8x8 group at luma (0, 32), `PARTITION_HORZ` -> two `BLOCK_4X8` |
| (12..15, 36..39) | (24..31, 36..39) | 8x8 group at luma (24, 32), `PARTITION_HORZ` |
| (4..7, 44..47) | (8..15, 44..47) | 8x8 group at luma (8, 40), `PARTITION_HORZ` |
| (12..15, 44..47) | (24..31, 44..47) | 8x8 group at luma (24, 40), `PARTITION_HORZ` |
| (0..3, 52..55) + (4..7, 52..55) | (0..15, 52..55) | 8x8 groups at luma (0, 48) and (8, 48), `PARTITION_HORZ` |
| (0..3, 60..63) | (0..7, 60..63) | 8x8 group at luma (0, 56), `PARTITION_HORZ` |

The blocks that *should* have written each hole did write their luma and their
top four chroma rows; the hole is always the **lower half of a 4x8 chroma plane
block**, never anything else.

### 1.3 Which arm, and what the tile walk does not code

Localised with a temporary probe (since removed) on the three
`PlaneBuf` sinks — every reconstruction store was recorded with its plane and
extent and the census's holes were attributed to owners:

```
SUB8_C422 lmi=(8,1)  leaf=(4, 8) cpx=0  cpy=32
SUB8_C422 lmi=(10,3) leaf=(4, 8) cpx=4  cpy=40
SUB8_C422 lmi=(8,7)  leaf=(4, 8) cpx=12 cpy=32
SUB8_C422 lmi=(10,7) leaf=(4, 8) cpx=12 cpy=40
SUB8_C422 lmi=(12,1) leaf=(4, 8) cpx=0  cpy=48
SUB8_C422 lmi=(12,3) leaf=(4, 8) cpx=4  cpy=48
SUB8_C422 lmi=(14,1) leaf=(4, 8) cpx=0  cpy=56
SUB8_C422 lmi=(14,3) leaf=(4, 4) cpx=4  cpy=56
SUB8_C422 lmi=(15,3) leaf=(4, 4) cpx=4  cpy=60
```

Every arm is `sub8_leaf_chroma422` (`decode.rs`, reached from
`decode_leaf_rect8`'s three chroma-reference call sites and from
`decode_leaf_split4`). Each writes one **4x4** chroma unit at `(cpx, cpy)`. The
seven `leaf=(4, 8)` entries are the chroma references of the seven
`PARTITION_HORZ` groups in §1.2, and `cpy` is the group's TOP row — the bottom
row is never visited, by any arm, on any plane.

---

## 2. Is this reachable on a stream this decoder is supposed to accept?

**Yes — the shape is legal AV1 syntax and a 4:2:2 stream can carry it. The
committed corpus happens to contain none.**

### 2.1 Corpus sentinel sweep (all 127 committed fixtures)

```
$ for f in crates/ec-av1/fixtures/*.obu; do \
    out=$(EC_AV1_PLANE_SENTINEL=1 EC_AV1_SENTINEL_CENSUS=1 timeout 120 \
            ./target/debug/examples/decode_probe "$f" 2>&1 >/dev/null | grep SENTINEL_CENSUS); \
    if echo "$out" | grep -qv 'unwritten=0'; then echo "=== $(basename $f)"; \
        echo "$out" | grep -v 'unwritten=0'; fi; done
=== 440_request_is_422.obu
SENTINEL_CENSUS idx=0 plane=U ... unwritten=112 ...
SENTINEL_CENSUS idx=0 plane=V ... unwritten=112 ...
```

126 of 127 fixtures — 4:2:0 (lossless, mixed-lossless, tall 8x16/16x8, odd
65x65, odd heights, altref, intrabc), 4:4:4 (lossless 128-root mu-chroma,
palette, rect4 444 at 8/10-bit), and 4:2:2 at 8/10/12-bit including
`Y_intrabc_10b.obu`, `X_intrabc_tiled.obu`, `W_intrabc.obu`, the `s422_*`
slices, grain and superres cells — have a **fully written** census on every
frame of every plane. Only the hand-built witness does not.

### 2.2 Why the corpus cannot see it: the defective shape has zero reach on valid streams

Same sweep, counting arm entries and how many carry the `leaf=(4, 8)` shape:

```
$ for f in crates/ec-av1/fixtures/*.obu; do \
    n=$(EC_RECON_TRACE=1 ./target/debug/examples/decode_probe "$f" 2>&1 >/dev/null | grep -c SUB8_C422); \
    if [ "$n" != 0 ]; then v=$(... grep -c 'SUB8_C422.*leaf=(4, 8)'); \
        echo "$(basename $f) c422=$n vert48=$v"; fi; done
422_inter_160x128_3f.obu               c422=29   vert48=0
422_key_64x64.obu                      c422=2    vert48=0
422_palette_intra_in_inter_384x240_17f c422=158  vert48=0
422_residual_compound_warp_16f.obu     c422=257  vert48=0
422_residual_compound_warp_nolr_16f.obu c422=256 vert48=0
440_request_is_422.obu                 c422=9    vert48=7   <-- the only one
W_intrabc.obu                          c422=391  vert48=0
X_intrabc_tiled.obu                    c422=333  vert48=0
Y_intrabc_10b.obu                      c422=539  vert48=0
s422_12bit_160x128.obu                 c422=185  vert48=0
s422_320x246.obu                       c422=162  vert48=0
s422_322x240.obu                       c422=195  vert48=0
s422_322x246.obu                       c422=155  vert48=0
s422_352x242_10b.obu                   c422=169  vert48=0
s422_416x242_10b.obu                   c422=176  vert48=0
s422_416x250_10b.obu                   c422=198  vert48=0
s422_grain_160x128.obu                 c422=45   vert48=0
s422_superres_160x128.obu              c422=45   vert48=0
```

3528 real `sub8_leaf_chroma422` invocations across the 4:2:2 corpus, **zero** of
the `leaf=(4, 8)` shape. So:

* the DEFECT is not corpus-faked — the arm is heavily used and every other shape
  is right;
* the defect is latent, not observed, on valid streams, because no committed
  4:2:2 stream codes an 8x8 block with `PARTITION_HORZ`.

**Latent does not mean absent.** AV1's 8x8 partition set is the full
`PARTITION_TYPES` (`partition_cdf_length`, `av1_common_int.h:1555-1560`: `bsize
<= BLOCK_8X8` gets all eight), so `PARTITION_HORZ` at 8x8 is codable, and libaom
decodes it through exactly the path below. The witness's tile is a genuine 4:2:0
key frame's tile, so the `PARTITION_HORZ` groups it carries are ordinary legal
partition symbols — the header/tile chroma mismatch changes only the geometry
they are decoded against.

### 2.3 What libaom does here (the derivation, from source)

1. `av1/decoder/decodeframe.c`, `decode_partition` -> `parse_decode_block`
   (`decodeframe.c:1229`) runs once **per leaf**, so a `PARTITION_HORZ` 8x8 group
   reaches `decode_token_recon_block(pbi, td, r, BLOCK_4X8)` once per leaf.
2. `decode_token_recon_block` (`:993-1041`) skips the chroma planes when
   `!xd->is_chroma_ref`. `is_chroma_reference` (`av1_common_int.h:1454-1461`) is
   `((mi_row & 1) || !(bh & 1) || !ss_y) && ((mi_col & 1) || !(bw & 1) || !ss_x)`.
   For `BLOCK_4X8` at ss (1,0): `bw=1` -> the column clause is `mi_col & 1`, so
   **only the odd-column leaf codes chroma**; the row clause is `true` because
   `!ss_y`. That is the crate's `!vert || i == 1` in `decode_leaf_rect8`, and it
   is right.
3. For the chroma reference leaf, `tx_size = av1_get_tx_size(1, xd)`
   (`blockd.h:1381-1388`) = `av1_get_max_uv_txsize(BLOCK_4X8, 1, 0)`
   (`blockd.h:1372-1379`) = `max_txsize_rect_lookup[get_plane_block_size(
   BLOCK_4X8, 1, 0)]`. `get_plane_block_size` takes the **per-axis MIN** of the
   two `av1_ss_size_lookup` entries, so `min(BLOCK_INVALID, BLOCK_4X8)` =
   `BLOCK_4X8`, and `max_txsize_rect_lookup[BLOCK_4X8]` (`common_data.h:126-127`)
   is **`TX_4X8`**. (At lossless, `av1_get_tx_size`'s first line returns `TX_4X4`
   on every plane, `blockd.h:1383`.)
4. The walk loop at `:1011-1033` with `tx_size = TX_4X8`:
   `max_block_wide/high` are in MI units (`blockd.h:1565-1594`) = 1 and 2;
   `tx_size_high_unit[TX_4X8] = 2`, `stepc = tx_size_wide_unit[TX_4X8] = 1`
   (`common_data.h:246-253`). `blk_row = 0` only, `blk_col = 0` only:
   **one `TX_4X8` unit covering the whole 4x8 chroma plane block.**

This decoder instead emitted a single `TX_4X4` (`read_plane(..., 4, TX4, ...)`
then `push_intra(..., 4, ...)`) — half the plane block. At lossless the existing
square model is the *correct* one (`TX_4X4` per unit, two stacked units), which
is why the arm's `leaf=(4, 4)` path and the `decode_leaf_split4` route are
untouched by this fix.

---

## 3. The fix

`crates/ec-av1/src/decode.rs`, `sub8_leaf_chroma422`, one new branch:

```rust
let rect48 = leaf_shape == (4, 8) && !lossless(fctx);
```

When it holds, the arm hands the PAIR's whole 4x8 chroma plane block to
`leaf8_chroma422_unit` — the port that already exists for a `BLOCK_8X8` 4:2:2
leaf (`TxbSet::ChromaRect8x4` + `SCAN_4X8` + `read_coeffs_rect(4, 8, ..)` +
`dequant_and_inverse_typed_wh(4, 8, ..)` + `push_intra_rect(.., 4, 8, ..)`, whose
own doc cites the identical `av1_get_max_uv_txsize` derivation). The two intrabc
shapes hand their frame copy over through that function's existing
`pred_override` parameter, which is how the `BLOCK_8X8` leaf passes the same two
cases. Alongside it, three sub-8-specific inputs follow the unit's new height:
the reach (`tx_h = 8 << ss_y`), the entropy context
(`around_mi_422_chroma(ctx_mi, 8, 8)`) and the CfL AC span
(`cfl_src_rect(floored_x, py, 8, 8)`).

The three existing square arms stay for `leaf=(4, 4)` (the `decode_leaf_split4`
route, one `TX_4X4` per odd-column leaf, which tiles 4x8 correctly) and for
lossless (two stacked `TX_4X4`, which is what libaom walks there).

**Why a real fix and not a detection/padding.** Refusal is not available: the
sequence header is self-consistent AV1 and the partition/mode syntax is
subsampling-independent, so the tile parses with no signal that the header and
the tile disagree — there is nothing to detect. Pre-filling the plane with 0 or
with a mid-grey would hide the class and still emit wrong pixels. Making the
walk cover the block it was handed is the only real answer, and the walk's own
libaom derivation says what "cover" is: one `TX_4X8`.

### 3.1 After the fix

```
$ EC_AV1_PLANE_SENTINEL=1 EC_AV1_SENTINEL_CENSUS=1 \
    ./target/debug/examples/decode_probe crates/ec-av1/fixtures/440_request_is_422.obu 2>&1 >/dev/null
SENTINEL_CENSUS idx=0 plane=Y extent=64x64 unwritten=0
SENTINEL_CENSUS idx=0 plane=U extent=32x64 unwritten=0
SENTINEL_CENSUS idx=0 plane=V extent=32x64 unwritten=0

$ for i in 1 2 3 4 5; do EC_AV1_FINAL_DUMP=/tmp/wt_aw/w$i \
    ./target/debug/examples/decode_probe crates/ec-av1/fixtures/440_request_is_422.obu >/dev/null 2>&1; \
    sha256sum /tmp/wt_aw/w$i.f0; done
3823d219ba99c28c904d3a4f6f695fe56fa6cd5b7f26da5919e525b36478d7df  ... (x5, identical)
```

Five runs, one hash.

---

## 4. The gate

`crates/ec-av1/src/stream.rs`, `a_422_header_over_a_420_tile_leaves_no_sample_unwritten`:

* pins the same fixture through the existing `read_pin` (2014 bytes,
  fnv1a64 `0x98a1378df976253d`);
* turns the sentinel on through the new `decode::set_plane_sentinel` (a thread
  local, because this crate snapshots the environment once per process and a
  gate cannot set a process-constant env var) and turns it off again through a
  `Drop` guard;
* decodes it and asserts `decode::take_unwritten_samples() == 0`;
* asserts `decode::take_census_scanned() >= 64*64 + 2*32*64`, so a census that
  scanned nothing cannot pass as a census that found nothing
  (`vacuous-test-assertions`).

```
$ cargo test -p ec-av1 --features gate-counters --lib a_422_header_over_a_420_tile_leaves_no_sample_unwritten
test stream::tests::a_422_header_over_a_420_tile_leaves_no_sample_unwritten ... ok
test result: ok. 1 passed; 0 failed; ...
```

### 4.1 Mutation: the gate bites

`let rect48 = false && leaf_shape == (4, 8) && !lossless(fctx);` — the fix
disabled, nothing else:

```
thread 'stream::tests::a_422_header_over_a_420_tile_leaves_no_sample_unwritten' panicked at
  crates/ec-av1/src/stream.rs:2912:9:
assertion `left == right` failed: a_422_header_over_a_420_tile_leaves_no_sample_unwritten:
224 samples of the frame were handed to the tile walk and never written, so they reach the output
as whatever the allocator left there (the five different dump hashes this pin used to give)
test result: FAILED. 0 passed; 1 failed
```

224 = 112 (U) + 112 (V), the §1.1 census exactly. Reverted, gate green again.

---

## 5. Regression evidence

* **Whole-corpus output equality, pre-fix vs post-fix.** Every fixture decoded
  with `EC_AV1_FINAL_DUMP` on both trees, all `.f*` files hashed: the two hash
  lists differ **only** for `440_request_is_422.obu`. Every 4:2:0 / 4:4:4 /
  4:2:2 committed stream produces byte-identical output, which is what §2.2's
  reach measurement predicts (the `leaf=(4, 8)` shape never fires on them).
* **Whole-corpus sentinel sweep re-run after the fix**: `non-clean fixtures:
  0 / 127`. The witness's 112+112 is gone and the other 126 are unchanged,
  which is what §2.2's reach measurement predicts.

---

## 6. What this lane did NOT do, and why

* No oracle comparison for the fix's pixels. The `TX_4X8` chroma unit is
  derived from libaom's source (§2.3) and reuses the crate's already-pinned
  port of the identical unit, but **no committed valid stream reaches the
  shape**, so there is no byte-exact claim to make and none is made. A 4:2:2
  stream whose 8x8 blocks code `PARTITION_HORZ` is the fixture this fix still
  wants; building one needs an encoder that emits that partition, which this
  crate's encoder does not.
* `film_grain.rs`, the reserved 4:2:0 group-tail chroma SKIP arm and
  `decode_rect4_16_intrabc` were not touched. The cause is not in them.
* The census instrument (`census_unwritten`, `set_plane_sentinel`,
  `take_unwritten_samples`, `take_census_scanned`) and the
  `PlaneBuf::plane` field are additions; the field is what lets the census and
  any future write-trace name the plane from the buffer instead of from the
  call site.