# lane-oddh r2 — the 64x72 first-symbol desync is 128x128 superblocks

## Root cause (byte-level proof, not inference)

r1's pinned defect — a 64x72 aomenc KEY frame desyncing at the FIRST symbol
(aomdec value=3, ours 9) — is **not** an odd-height defect and **not** a header
parse defect. The stream's sequence header codes `use_128x128_superblock = 1`,
and this decoder is 64x64-superblock only (`SB_MI = 16`, no BLOCK_128X128
partition read anywhere; `decode.rs` never reads `use_128x128_superblock`).

libaom picks it: `av1_select_sb_size` (encoder_utils.c:1000-1003) returns
BLOCK_64X64 only when `speed >= 1 && min(w,h) <= 480`. r1's fixture was
encoded at `--cpu-used=0`, so speed 0 fell through to BLOCK_128X128.

What the two decoders do with the same first byte:

| | libaom (aomdec) | ours |
|---|---|---|
| first block | BLOCK_128X128 at mi (0,0): `has_cols = 0+16 < 16` false, `has_rows = 16 < 18` true | BLOCK_64X64 at mi (0,0) |
| symbol read | gathered 2-symbol partition (`partition_gather_horz_alike`) | 10-symbol `partition_cdf[12]` |
| value | 3 = PARTITION_SPLIT | 9 = PARTITION_VERT_4 |

Ruled out first, by hand, field by field against spec 5.9.2 for this stream:
the uncompressed header is **49 bits** (3 before tile_info, 1 tile_info, 12
quant, 1 seg, 1 delta, 29 loop filter — levels 36/38/21/19, sharpness 0,
mode_ref_delta_enabled 1, update 0 — 0 cdef, 0 lr, tx_mode 1, reduced_tx_set 1),
byte-aligning tile data to file offset 20. Our parser reads exactly the same 49
bits. Two independent msac implementations (ours and a from-spec python one)
read symbol 9 from offset 20 with `partition_cdf[12]`, so the bytes and the
table are both ours-correct — only the *block size* is wrong.

EVIDENCE: hand-parsed header bits + msac offset probe (both diagnostics reverted after use) | parsed k-64x72.obu, printed per-section bit positions (3/4/16/17/18/47/47/47/49), then decoded the first partition symbol at file offsets 16..24 with `PARTITION_W64[0]` in ours and in a from-spec python msac | both give 9 at offset 20 (aomdec: 3), i.e. same bytes, same table, different alphabet

## What changed

- `crates/ec-av1/src/stream.rs:509` — `decode_stream` now REFUSES a sequence
  with `use_128x128_superblock` **on a frame larger than 64x64** by name
  instead of silently decoding it as 64x64 superblocks (wrong pixels, whole
  frame). The scoping is measured, not reasoned: a blanket refusal red-lined
  29 previously pixel-exact gates (29 failed / 318 passed), because at
  <= 64x64 a 128x128 superblock decodes identically -- mi (0,0) has neither
  `has_rows` nor `has_cols`, so BLOCK_128X128 is a forced SPLIT that reads no
  symbol and only one 64x64 child is in frame. Every sibling inter gate at
  64x64 `--cpu-used=0` is exactly that case.
- `crates/ec-av1/src/stream.rs` — new gate
  `a_real_aomenc_128x128_superblock_stream_is_refused_by_name`: real aomenc,
  64x72, `--cpu-used=0 --sb-size=128`; asserts the stream's sequence header
  really codes `use_128x128_superblock` (knob-reached-the-tool) and then that
  the refusal fires.
- `crates/ec-av1/src/stream.rs` — `a_real_aomenc_tiny_frame_size_sweep_decodes_pixel_exact`
  extended with the odd ladder 64x72, 72x64, 64x136, 136x64, 192x136; floor
  raised 70 -> 120 pixel-exact attempts.
- `crates/ec-av1/src/refusal_inventory.rs` — the new refusal string pinned.
- `crates/ec-av1/src/stream.rs` — `a_real_aomenc_inter_frame_edge_half_strip_decodes_pixel_exact`
  written and left `#[ignore]`d with its measured reason (below).

## The size ladder (the table the charter asked for)

`cargo test -p ec-av1 --lib -- a_real_aomenc_tiny_frame_size_sweep_decodes_pixel_exact`,
10 seeds each, real aomenc at `--cpu-used=4` (= 64x64 superblocks), 8-bit:

| size | pixel-exact | refused |
|---|---|---|
| 8x8 | 0/10 | 10 (16x16 rect transform, pre-existing) |
| 16x16, 32x32, 16x32, 32x16, 64x64, 48x48, 24x24 | 10/10 each | 0 |
| **64x72, 72x64, 64x136, 136x64, 192x136** | **10/10 each** | 0 |

EVIDENCE: $HOME/.cache/oddh-suite-r2.log + the run above | 13 sizes x 10 seeds, aomenc -> our decoder -> ffmpeg sample compare | 120 pixel-exact attempts, 0 mismatches, "tiny frame sweep: 120 pixel-exact attempts"

So odd height/width at 64x64 superblocks is **clean**, including r1's
frame-edge half-strip code on the KEY path.

## Open / deferred

- deferred: 128x128 superblock decode — refused by name, not implemented — the
  partition tree, `SB_MI`, CDEF/LR/deblock grids and the loop-filter unit size
  are all 64-only; that is a lane, not a fix. Unblocked by: a lane that makes
  `SB_MI` a parameter and adds the BLOCK_128X128 partition read.
- deferred: the INTER frame-edge half-strip arms r1 added are still UNGATED.
  Measured this round at 192x160 (last superblock row 32 tall, so the forced
  gathered edge symbol is a 64x32): with `--enable-rect-partitions=0` both bit
  depths decode 6 frames **pixel-exact** but `edge_rect_strip_hits = 0` (the
  encoder answers the forced symbol SPLIT every time); with rect ON the run
  stops at other lanes' gaps — cq 40 "a non-skip rectangular (HORZ/VERT/HORZ_B)
  strip needs rectangular residual coding", cq 62 "an INTER 32x32 partition
  type this decoder does not code (value=8/9)", min-partition 16 "a HORZ/VERT
  intra strip below 16x16 with a split transform". Unblocked by: the rect
  inter-residual lane.

## Suite

`cargo test -p ec-av1 --lib` (systemd unit, log `$HOME/.cache/oddh-suite-r2.log`):
the blanket-refusal run was 318 passed / 29 failed / 32 ignored, every failure
the new refusal firing on a 64x64 `--cpu-used=0` stream. After scoping the
refusal to frames larger than 64x64, the 7-gate subset covering all of them
plus the three new/extended gates is green (7 passed, 0 failed); the full-suite
rerun on the scoped tree is launched to the same log and is r3's first read.
