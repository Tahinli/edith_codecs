# lane/av1unwritten — a MISSING REFUSAL, not an unwritten chroma unit

**Outcome in one line: `440_request_is_422.obu` is a 4:2:2 sequence header over a
4:2:0 tile, so its partition symbols name subsizes (8x16 among them) with NO
chroma plane block at ss (1,0) — libaom calls that `AOM_CODEC_CORRUPT_FRAME`
("Block size 8x16 invalid with this subsampling mode") and `aomdec` refuses the
file outright; we walked those subsizes, and the nondeterministic output was the
symptom. The fix mirrors libaom's own check and returns a named refusal; the
sentinel census that found the class is kept as the instrument.**

Branch `lane/av1unwritten`, base `main` = `8d6998d7`. No push.

> **This report was rewritten.** The first version of this lane claimed a libaom
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

## 2. What the symptom looked like, and how the census found it

`440_request_is_422.obu` is what `examples/gen_coverage_cells.rs` produces when
the writer is handed `subsampling (0, 1)` at profile 2: the header lands on 4:2:2
and the frame OBU behind it is a real 4:2:0 key frame's, byte for byte. The
header/tile chroma mismatch is exactly what produces a subsize whose plane block
does not exist.

```
$ for i in 1 2 3 4 5; do EC_AV1_FINAL_DUMP=/tmp/wt_aw/u$i \
      ./target/debug/examples/decode_probe crates/ec-av1/fixtures/440_request_is_422.obu \
      >/dev/null 2>&1; sha256sum /tmp/wt_aw/u$i.f0; done
de5051156b7cfd00dc7ed5f2c6ad404e2a49822a8a728bc027a938aac00219f1  /tmp/wt_aw/u1.f0
eb0e513ff07d9780b03e70861d1a7f507f540e4724f62384511a71e06c386aa9  /tmp/wt_aw/u2.f0
2985b82874579807efb8bc88136d1b8f5f1f0121410cde26723d5be7e6ba8715  /tmp/wt_aw/u3.f0
204645ad20d7e557d7b04f0027d9aafd5790fe685dc39bdf7cfb117d9dd1f0fc  /tmp/wt_aw/u4.f0
80908a1712c4fe78070a6a3ad57464e8bd6a9d0b8d73e86e8f9cf7ea7cbd3496  /tmp/wt_aw/u5.f0
```

Five hashes from one file, with every census count identical — the difference was
purely the samples the walk never wrote.

`EC_AV1_PLANE_SENTINEL=1` (the census has no separate flag) plus the new
`EC_AV1_SENTINEL_CENSUS` printout — the shipped form of the four-line temporary
scan `lanes/av1422seed.report.md` §3 used and explicitly did not commit — gave an
exact census at each frame's pre-deblock point:

```
SENTINEL_CENSUS idx=0 plane=Y extent=64x64 unwritten=0
SENTINEL_CENSUS idx=0 plane=U extent=32x64 unwritten=112 spans=24: (six 4x4 blocks)
SENTINEL_CENSUS idx=0 plane=V extent=32x64 unwritten=112 spans=24: (identical)
```

Whole 4x4 chroma blocks, and each was the lower half of the plane block of one of
the subsizes §1 says are not decodable. That is the whole chain: the walk
accepted a subsize libaom refuses, and the chroma plane block it then covered
was shaped differently from the luma one.

The instrument is **kept**: it is env-gated, off by default, and it is the only
way to ask "did the tile walk write every sample it was handed" (a wrong-sample
diff cannot tell an unwritten sample from a mis-predicted one).

---

## 3. The fix

`crates/ec-av1/src/decode.rs`:

* `chroma_plane_block_codable(bw, bh, ss_x, ss_y)` — the four
  `BLOCK_INVALID` cells of `av1_ss_size_lookup`, nothing else.
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

### 3.1 Sites found, and the ones proven not to need it

| site | subsizes it can produce | needs the check? |
|---|---|---|
| `partition_w8` (2 sites, 36549 / 36767 region) | 8x4, 4x8 | **yes**, installed |
| `partition_w16` intra (36175 region) | 16x8, 8x16 | **yes**, installed |
| `partition_w16` edge split (`VERT_ALIKE`/`HORZ_ALIKE` gather) | same two, reached only when `has_cols16`/`has_rows16` is false | covered by the same rule at the sibling site; the edge arm's two-symbol gather can only choose SPLIT vs the matching strip |
| `PARTITION_HORZ_4` / `VERT_4` on 8x16 / 16x8 (`decode_rect_split`) | 4x16, 16x4 | **NOT installed — known gap, stated here** |
| 32/64-level partitions, 128 root, AB partitions | squares and 32x64/64x32 | no: every cell of those rows is non-INVALID at every subsampling this decoder accepts |
| 4:4:4 square leaves, `decode_leaf8`, `decode_leaf_split4` | BLOCK_4X4 / BLOCK_8X8 / BLOCK_16X16 | no: `(0,0)` column is non-INVALID for all of them |

The 1:4 sites are the honest gap: `PARTITION_HORZ_4` on an 8x16 yields a 4x16,
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
  refuses. Measured by the same loop as the §2 hashes, with the per-fixture
  stderr scanned for `REFUSED`.
* **Sentinel sweep** (127 fixtures, `EC_AV1_PLANE_SENTINEL=1`): pre-fix exactly
  one fixture had a non-empty census (the witness, 112+112); post-fix the corpus
  has no unwritten sample anywhere — the witness decodes nothing at all now, and
  the other 126 are unchanged.
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

**Determinism, now a consequence rather than the pinned property.** The witness
no longer produces output at all, so there is no hash to vary. Before the fix,
`for i in 1..5` gave five hashes (top of §2); after, the file refuses. The
nondeterminism was never a property of the pin worth pinning — it was the shape
of a stream we should not have decoded at all.

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
