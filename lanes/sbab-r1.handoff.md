# lane-sbab r1 handoff — superblock-level inter AB partitions

Branch `lane-sbab`, tip `038d1f8` (rebased onto main `cc323d0`).
Worktree `/home/tahinli/Documents/Code/Rust/edith_codecs-sbab`.

## 1. Real, not our own desync

`EC_TRACE=1 aomdec` on `~/.cache/sbab-tmp/s_8_3_y_45.obu` (256x192, 8-bit, cq45,
Y-structured source, sp=3) prints, in decode-order frame 4 (an INTER frame; the
stream has 12 SB partition reads per frame, entries 49..60 are frame 4):

```
EC_PART_VAL mi_row=16 mi_col=32 bsize=12 value=4   # PARTITION_HORZ_A at 64x64
EC_PART_VAL mi_row=32 mi_col=32 bsize=12 value=5   # PARTITION_HORZ_B at 64x64
```

bsize=12 is BLOCK_64X64 and values 4/5 are HORZ_A/HORZ_B, so the refusal
"an inter SB-level AB partition" named a real stream shape. VERDICT: REAL.

## 2. What is implemented

`crates/ec-av1/src/decode.rs:21494`, in `decode_inter_frame_tile_with_cdfs`'s
64-level partition switch: the four arms in libaom `decode_partition` order
(`decodeframe.c`) —
HORZ_A = TL 32x32, TR 32x32, bottom 64x32 strip;
HORZ_B = top 64x32 strip, BL 32x32, BR 32x32;
VERT_A = TL 32x32, BL 32x32, right 32x64 strip;
VERT_B = left 32x64 strip, TR 32x32, BR 32x32.
The squares reuse the `PARTITION_NONE`-under-SPLIT `decode_inter_block` call
verbatim (Luma32/Luma32Inter/Chroma16, TX32/TX16, size group 3, side `BLOCK`);
the strips reuse `inter_piece!` (side `SB`, true footprint via `write_w`/
`write_h`), which lane-r14's `superblock_level_rect_partition` gate proves.
`Reach::vert_ab_partition` is armed for the two VERT arms only (visit order
TL,BL,TR,BR changes `has_tr`/`has_bl`). Per-arm counter
`SB_AB_INTER_HITS` / `sb_ab_inter_hits_by_arm()` (decode.rs:2222), printed by
`examples/decode_probe` as `inter_ab64:`.

## 3. The defect the gate found (root cause, shared path)

`decode.rs:19128` — `decode_inter_block`'s var-tx branch called
`record_split_luma_rect_mi(at, side, side, ...)` with the comment "a var-tx
block is always square, so write_w == write_h == side". That premise died when
the SB-level rect strips landed (lane-inter4): a 64x32 / 32x64 strip keeps
`side = SB` for syntax but is not square, so it stamped `above_side_mi = 64`
where libaom's `update_partition_context` writes
`partition_context_lookup[BLOCK_32X64]`. The superblock BELOW then computed
partition ctx 2 where aomdec reads 3, wrong CDF row, desync at the next symbol.
Fixed by passing `write_w, write_h`. Ladder evidence, frame 4 of the stream
above, `EC_AV1_TRACE=1` ours vs `EC_TRACE=1` aomdec (aomdec ctx prints as
`4*bsl+ctx` = 12+ctx):

| mi        | oracle ctx | ours before | ours after | oracle pre_rng | ours after |
|-----------|-----------|-------------|------------|----------------|------------|
| (16,48)   | 3         | 2           | 3          | 49168          | 49168      |
| (32,0)    | 0         | 0           | 0          | 53403          | 53403 (was 60557) |
| (32,32)   | 2         | 0           | 2          | 42672          | 42672 (was 32906) |

After the fix all 72 partition reads of the stream match the oracle in ctx,
value and range.

Note this is a SHARED-path fix: it changes every var-tx inter block whose
footprint is not square, i.e. the SB-level HORZ/VERT/HORZ_4/VERT_4 strips too.
Sibling coverage is the full suite run (below), not this gate alone.

## 4. Gate

`a_real_aomenc_inter_sequence_with_superblock_level_ab_partitions_decodes_pixel_exact`
(`crates/ec-av1/src/stream.rs:8375`). 256x192, 6 frames, 20 attempts per bit
depth (cq 18/26/34/45/55 x two source orientations x motion step 3/8), aomenc
`--sb-size=64 --enable-rect-partitions=1 --enable-ab-partitions=1
--enable-1to4-partitions=0 --min-partition-size=32 --max-partition-size=64
--kf-max-dist=9999 --cpu-used=0 --threads=1 --row-mt=0`, every decode-order
frame compared Y/U/V vs ffmpeg, `oos_mismatch` asserted 0, all four arms
asserted to have fired on a pixel-exact attempt.

```
(8-bit):  0 named refusals, 12 pixel-exact attempts carrying a 64-level inter AB
          partition, per-arm HORZ_A/HORZ_B/VERT_A/VERT_B=[6, 5, 4, 2],
          8 attempts carried none (0 of them mismatched)
(10-bit): 0 named refusals, 4 pixel-exact attempts, per-arm=[2, 1, 3, 2],
          16 attempts carried none (0 of them mismatched)
test result: ok. 1 passed; 0 failed; 418 filtered out; finished in 92.12s
```

## 5. Refusal state

Removed from `refusal_inventory.rs`: `"an inter SB-level AB partition
(HORZ_A/HORZ_B/VERT_A/VERT_B; ...)"`, with a comment naming the gate that
replaced it. No arm stayed unreachable — all four fired live at both depths, so
nothing is left refused by name here. `gate_coverage.rs` needs no change
(`enable-ab-partitions` was already covered).

## 6. Open residue

- deferred(below-8x8 inter leaf desync): a `--tile-columns=1` twin of the 20
  attempts PANICS in `mc::from_switchable_symbol` (`mc.rs:203`, a 4th symbol
  out of a 3-symbol alphabet) on 8-bit attempt 27 — the exact signature
  lane-inter16ab r1 recorded for its own tiled arm. Not this lane's shape; the
  tiled axis is excluded with that reason in the gate source rather than
  SKIPped. Unblocked by whoever lands the sub-8 inter leaf.
- The 64-level 1:4 pair (HORZ_4/VERT_4 at 64) is unrelated and already decoded;
  `--enable-1to4-partitions=0` here only keeps the sweep narrow.

## 7. Exact next step

1. Read `$HOME/.cache/sbab-suite-r1.log` (`systemd-run --user` unit
   `sbab-suite-r1`, armed at commit `038d1f8`): confirm N passed / 0 failed.
   The var-tx extent fix is repo-wide, so a red sibling there is this lane's to
   own, not another lane's.
2. If green, merge `lane-sbab` into main and delete the worktree.
3. Optional follow-up, not started: film check with the 0.4 s extracts — this
   lane never ran one.
