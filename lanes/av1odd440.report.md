# lane-av1odd440 — the two unreachable cells, one closed and one dissolved

Branch `lane-av1odd440`, base `70ed5601` (main). Target dir
`$HOME/.cache/tgt/av1odd440`. No push.

## Verdict, in four lines

1. **`4:2:0` with an ODD luma dimension: DECODED, pixel-exact, now gated.** The
   stream aomenc cannot make exists, decodes byte-identical to `aomdec`
   (6403 raw bytes, 65x65 + 2x33x33), and the gate bites: flooring
   `decode::round_ss` turns it red.
2. **The `4:4:0 (ss 0,1)` row is not a decoder gap — it is not a cell.** AV1
   codes `subsampling_y` only when `subsampling_x` is 1, so the reachable set
   is exactly `{(1,1), (1,0), (0,0)}` and `(0,1)` is uncodable. libaom's own
   writer asserts it in a string: `"4:4:0 subsampling not allowed in AV1"`.
3. The odd-luma fixture exposed a **real decoder defect** the tree had
   documented as an open gap and deferred: `ref_chroma_shape`'s 4:2:0 arm was a
   floor. It is now the producer's own `round_ss` crop, red-before proven.
4. The decoder **still refuses 4:2:2 by name**; the refusal string now says
   why 4:4:0 cannot arrive either, and the inventory's two copies of it are
   updated with the same text.

## The two cells

### Cell 1 — `420_odd65x65_key.obu` (4:2:0, odd luma): `y`

| | |
|---|---|
| bytes | 2305 |
| sha256 | `66986019a561f81130f407f426c4379b25e6e80c2cd9ce47d517b3216e9a03cf` |
| fnv1a64 | `0xf55645c4b139e8f7` |
| header | `seq_profile 0`, `max_frame 65x65`, `subsampling (1,1)`, 8-bit |
| ours vs `aomdec` | **6403 == 6403 bytes, identical** (65*65 + 2*33*33) |

Reproduce (no encoder, no ffmpeg, no lavfi — the whole point of the cell):

```
cargo run -p ec-av1 --example gen_coverage_cells -- crates/ec-av1/fixtures
sha256sum crates/ec-av1/fixtures/420_odd65x65_key.obu
```

`aomenc` cannot produce this stream: it rounds the coded size DOWN to even
(the formatsweep lane measured `testsrc2 131x131` y4m coming out as
`max_frame=130x130`), so the encoder had to be told what size to DECLARE.
`encode::encode_key_frame_at_size` is that entry, and the fixture is a 96x96
card padded to the block grid with a 65x65 frame header.

**Why the gate is not a rubber stamp.** The cell's whole content is the chroma
extent: a 65-wide 4:2:0 frame has `ROUND_POWER_OF_TWO(65, 1) = 33` chroma
columns (libaom `av1_common_plane_width`, `av1/common/av1_common_int.h`), not
32. Measured mutations:

| mutation | result |
|---|---|
| `round_ss` floored (`dim >> ss`, decode.rs:697) | gate RED: `left (1024, 1024) right (1089, 1089)` (32x32 chroma); the raw dump is 6273 bytes against the oracle's 6403 |

**Defect it exposed, fixed here.** `decode::ref_chroma_shape`'s 4:2:0
fallback arm was `(width / 2, height / 2)` — a floor. The producer stores
`round_ss(w,1) x round_ss(h,1)`, so every odd-dimension 4:2:0 REFERENCE was
under-declared (12160 of 16384 `(w,h)` pairs over `1..=128`, per the
function's own doc comment, which deferred the fix to "a lane that can bring
odd-dimension 4:2:0 fixtures with it"). The arm is now the producer's crop.
Red-before, measured: with the floor back, the existing routing test
`a_reference_chroma_sample_count_routes_back_to_its_own_format` fails at
`4:2:0 reference of 2x3 ... left (1, 1), right (1, 2)`. Even sizes are
untouched — `(2k+1)/2 == k` — so no existing 4:2:0/4:2:2/4:4:4 reference
changes shape.

### Cell 2 — `440_request_is_422.obu` (4:4:0): the cell does not exist

| | |
|---|---|
| bytes | 2014 |
| sha256 | `4afaa5470c4e010d9d970086ef514680adfe801a579e4deeec582f52d13c5e7c` |
| fnv1a64 | `0x98a1378df976253d` |
| what it is | a 4:4:0 REQUEST at profile 2, which lands on `(1,0)` = 4:2:2 |
| decoder | refuses by name (the 4:2:2 refusal) |

Reproduce: same `cargo run` as cell 1.

Spec 5.5.2 `color_config` codes `subsampling_y` ONLY when `subsampling_x` is
1. The two places libaom says so:

- reader — `av1/decoder/decodeframe.c:4171-4175`:
  `subsampling_x = aom_rb_read_bit(rb); if (subsampling_x) subsampling_y = aom_rb_read_bit(rb); else subsampling_y = 0; // 444`
- writer — `av1/encoder/bitstream.c:2466-2468`:
  `assert(seq_params->subsampling_y == 0 && "4:4:0 subsampling not allowed in AV1");`

So the coverage-matrix row was a MATRIX bug, not a decoder gap: aomenc has no
`yuv440p` input path and ffmpeg's y4m muxer refuses the format, but those are
consequences — no bit pattern anywhere produces `(0,1)`. There is no stream to
decode, no fixture a decoder could be handed, and no oracle to compare
against. The pin therefore carries the byte-level evidence instead: hand the
writer `(0,1)` and it emits a header that reads back `(1,0)`.

The gate `the_440_cell_is_not_a_codable_chroma_shape` proves the claim in
three arms, and every arm has a measured mutation:

| arm | what it measures | mutation | result |
|---|---|---|---|
| 1 WRITER | every `(profile, bit_depth, mono, colour description, requested shape)` combination a `color_config` can carry -- 180 of them, the profile-1-monochrome request skipped because the writer refuses it -- written and read back; the reachable set must be exactly `{(1,1),(1,0),(0,0)}`; and with `subsampling_x == 0` a `(0,0)` and a `(0,1)` request must write BYTE-IDENTICAL headers | writer emits the `subsampling_y` bit anyway | RED: `(0,0)`/`(0,1)` requests differ, `[..., 158, 2]` vs `[..., 158, 18]` at profile 2, 12-bit |
| 2 READER | the byte carrying `subsampling_x` is located by diffing an `(0,0)` against an `(1,0)` header, then all 256 values of it are read back | reader always reads a `subsampling_y` bit | RED: `payload byte 11 = 0x10 read back as 4:4:0` |
| 3 PIN | the pinned bytes read back `(1,0)`, not `(0,1)`, and still refuse by name | — (byte pin) | green |

Measured and recorded because it is the non-obvious half: mutating the
**writer** alone does NOT put `(0,1)` into a round trip — the correct reader
absorbs the extra bit as `separate_uv_delta_q`. The round-trip set is a
property of the writer/reader PAIR; the byte-identity arm is what pins the
writer's own half, and the reader arm is what pins the half a foreign stream
meets. Both mutations were run.

## Files

| file | change |
|---|---|
| `crates/ec-av1/examples/gen_coverage_cells.rs` | NEW — the generator, both cells |
| `crates/ec-av1/src/encode.rs` | NEW `encode_key_frame_at_size`; `crop_encoded` chroma extent is `div_ceil(2)`; `key_frame_headers_colour` made public |
| `crates/ec-av1/src/decode.rs` | `ref_chroma_shape` 4:2:0 arm floor -> `round_ss` crop; its routing test's expectation moved with it |
| `crates/ec-av1/src/stream.rs` | two new gates; the 4:2:2/4:4:0 refusal string |
| `crates/ec-av1/src/refusal_inventory.rs` | both copies of that string + the gate pairing |
| `crates/ec-av1-syntax/src/sequence.rs` | UNCHANGED (mutated and reverted only) |
| `crates/ec-av1/fixtures/420_odd65x65_key.obu`, `.../440_request_is_422.obu` | NEW pins (`git add -f`; `fixtures/` is gitignored) |

## Gates

- `an_odd_luma_420_key_frame_decodes_pixel_exact` — pin, header read from the
  pin's own bytes, decoded extents (65x65 luma, 33x33 chroma), then
  `assert_rawvideo_matches` against the instrumented `aomdec` (skips loudly
  without an oracle; hard-fails under `EC_AV1_REQUIRE_AOMDEC`).
- `the_440_cell_is_not_a_codable_chroma_shape` — the three arms above.
- `a_reference_chroma_sample_count_routes_back_to_its_own_format` — existing,
  its 4:2:0 expectation moved from the floor to the producer's crop.

## not_done

1. **No end-to-end odd-size INTER frame.** `ref_chroma_shape` is the
   reference path, and this fixture is a single key frame because that is all
   the ticket asked for and all the encoder's public entry points allow
   (`encode_sequence` derives the declared size from the source picture and
   `check_even`s it). The fix is red-before proven at the routing level and
   the pin is red-before proven at the decode level, but no odd-size inter
   stream decodes pixel-exact end to end, because producing one needs a sized
   sequence entry (`encode_key_frame_at_size`'s inter sibling) that this lane
   did not add. Whoever wants the inter arm should add that entry; the gate
   then only needs a second pin.
2. **The `ref_chroma_shape` `w == 1` column was measured, not witnessed.** The
   ceil arm expresses `(1, round_ss(h,1))`, which the floor could not, so the
   documented 127-pair residue is closed by construction; no stream this
   decoder accepts has a 1-pixel-wide frame to test it on.
3. **The 4:2:2 refusal is unchanged and still has no committed pixel witness**
   (every committed 4:2:2 pin asserts refusal, not pixels; the decode
   evidence needs the `EC_AV1_ALLOW_422_PROBE` patch-run-restore build, per
   `lanes/av1422bigblock.report.md`). This lane did not touch that.
4. **The 4:4:0 coverage-matrix row itself lives in
   `lanes/av1formatsweep.report.md`**, a historical report this lane does not
   edit. The correction is recorded here and in the refusal string; whoever
   maintains the matrix should delete the row rather than mark it `–`.
5. **No 12-bit or multi-frame odd-size variant.** One 8-bit key frame is the
   cell; a 65x65 stream at 10/12 bits would need the encoder widened first
   (it is 8-bit by design, `encode.rs`'s r2 decision).
