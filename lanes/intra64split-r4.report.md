# lane-intra64split r4 — RED for the witness, GREEN for the merge and the frontier measurement

## Verdict
RED on the GOAL. There is still NO firing, pixel-exact witness for the 64x32 /
32x64 depth-0 intra strip. On the most complete tree that exists (main plus all
eight un-merged inter lanes), the shape fires ONLY inside frames that then
refuse — every decodable prefix of every measured film cut has
`rect64_corner_tu = 0/0`. The gate arms therefore stay `#[ignore]`d exactly as
r3 left them, with every arm, recipe and pixel assert intact. Nothing was
weakened.

GREEN on what was asked besides the witness: the eight-lane merge builds, and
the film frontier is re-measured on that tree (it moved a long way at `-ss 0`).

## What changed
* Merge of `main` (1176a16) + eight lane tips not on main: `a5b9770`
  (intersub8), `7f372b9` (uv8), `10b801a` (interp3), `2e711e1` (inter16ab),
  `48216c2` (rectchroma2), `fedb7fe` (sb128c), `e0c8bef` (intra14),
  `a37a4bc` (intrasplit). Nine merge commits, `513d20b`..`0fea9e3`.
  Non-trivial resolutions, one copy kept of every duplicated fix:
  - `refusal_inventory.rs`: intersub8's lift of "an inter partition below 8x8"
    is authoritative — inter16ab re-added the string from an older base and it
    was dropped again.
  - `decode.rs` OBMC sentinel refusal (interp3 vs uv8): kept main's wording,
    one refusal, one string.
  - `decode.rs` wedge row selection: took inter16ab's
    `bsize_all_index`/`wedge_used_bsize`/`is_any_masked_compound_used_here`
    consistently (table and reader from ONE branch, class
    `table-and-reader-move-together`); intersub8's `bsize_index`/`wedge_used_wh`
    twin of the same fix was dropped.
  - `cdf_state.rs`: `TxbSet::LumaRect8x4Inter` was defined by BOTH intersub8 and
    inter16ab with identical semantics (Luma8 coefficient tables, 32-position
    `eob_pt`, `inter_tx_type_4`); one definition kept, inter16ab's
    `LumaRect8x4Inter1` kept alongside intersub8's `LumaRect8x4InterSet1`.
  - `decode.rs` rect-strip intra reader: intra14 wrapped the key-frame prologue
    in an `else` arm; main's `use_intrabc` read was re-placed INSIDE that arm
    (inter frames never have `allow_intrabc`), so no path lost the symbol.
  - `stream.rs`: intrasplit reorders two existing gates; HEAD's ordering taken
    for all eight hunks, both gates intact, one copy each.
* `crates/ec-av1/examples/decode_probe.rs` — prints
  `rect64_corner_tu: 64x32=.. 32x64=..` (commit `aebf4f8`), so a film sweep
  reads the witness counter without a test-binary rebuild. This is the entry
  surface every measurement below was driven through.

## MEASURED — the film frontier on the merged tree (2160p 10-bit HDR10 AV1, 2 s `-c:v copy` cuts)
`ffmpeg -ss S -t 2 -c:v copy -an -f obu`, then
`decode_probe` with `EC_AV1_FINAL_DUMP=<dir>/f`, under
`systemd-run --user --scope -q -p MemoryMax=6G`.

| start_s | frames dumped | stop string | rect64_corner_tu (64x32/32x64), cumulative incl. the refused frame |
|---|---|---|---|
| 0    | **33** | an inter SB-level AB partition (HORZ_A/HORZ_B/VERT_A/VERT_B; this decoder's inter tile path codes a superblock as NONE, SPLIT, HORZ, VERT, HORZ_4 or VERT_4) | 14 / 1 |
| 300  | 1 | a non-skip rectangular (HORZ/VERT/HORZ_B) strip needs rectangular residual coding | 0 / 2 |
| 1200 | 1 | an intra 8x4/4x8 block inside an inter frame's sub-8x8 HORZ/VERT partition (this decoder codes only inter sub-blocks there) | 0 / 3 |
| 1800 | 1 | a COMPOUND_WEDGE mask on a non-square inter block (rect wedge codebook unimplemented) | 5 / 7 |
| 3000 | 1 | an intra 16x4/4x16 strip inside an inter 16x16-level 1:4 partition (its 4:2:0 chroma pair is coded once for two strips; only the inter path implements that pairing) | 0 / 5 |
| 3600 | 1 | a non-skip rectangular (HORZ/VERT/HORZ_B) strip needs rectangular residual coding | 7 / 1 |

The `-ss 0` cut is the frontier move of this round: r3 measured ONE completed
frame at every offset; the merged tree completes 33 at `-ss 0` and stops on an
inter superblock-level AB partition, a refusal no lane in this batch owns.

EVIDENCE: <scratchpad>/i64r4/p{0,300,1200,1800,3000,3600}.log + d0/f* | ffmpeg -ss S -t 2 -c:v copy -f obu, decode_probe with EC_AV1_FINAL_DUMP | frames dumped and stop string per row, `rect64_corner_tu` from the probe's own line

## MEASURED — every hit is inside a refused frame (why there is still no witness)
Truncation sweep of the `-ss 0` cut with `census4/trunc.py` (keep the first N
frame-carrying OBUs), 21 points over N = 1..73:

* N = 1..49 (147 .. 4161 bytes): `decode_probe` returns without a refusal and
  `rect64_corner_tu = 0 / 0` at EVERY point.
* N = 50 (23472 bytes): the first refusal appears, and the counter jumps to
  14 / 1 in the same run. N = 60/66/70/73 add nothing (14 / 1 throughout).

So all 15 hits belong to frame OBU 50, the frame that refuses — class
`counter-from-refused-stream`. Truncating to "the minimal TU count" is
impossible here: there is no truncation that both decodes and carries the shape.

The key frame of every other cut was checked directly (`trunc.py <cut> 1`):
`-ss 300/1200/1800/3000/3600` all decode their key frame with no refusal and
`rect64_corner_tu = 0 / 0`. No key frame in the sweep carries the shape either.

EVIDENCE: <scratchpad>/i64r4/t{1..73}.obu, k{300,1200,1800,3000,3600}.obu | census4/trunc.py then decode_probe per truncation | N<=49 refused=0 hits 0/0; N=50 refused=1 hits 14/1; all five key frames refused=0 hits 0/0

## Fixture and gate
No fixture was added: a stream that only carries the shape in a frame we cannot
decode is not a witness, and pinning it would gate nothing (class
`gate-skips-on-its-own-failure` / `counter-from-refused-stream`). The r3 gate
arms stay `#[ignore]`d with r3's dated reason, and r3's tripwire
(`assert!(hits == 0)` on the pixel-exact synthetic recipe) is untouched, so the
demotion still cannot outlive its premise.

## Refusals
None lifted, none added by this round. The inventory is the union produced by
the eight-lane merge.

## Test totals
`cargo test -p ec-av1 --lib` as user unit `intra64split-suite-r4b-1788350949`,
log `$HOME/.cache/intra64split-suite-r4.log` --
**`test result: FAILED. 405 passed; 3 failed; 39 ignored; 0 measured` in 798s.**
RED. All three failures are MERGE CROSS-PRODUCT reds (class
`merge-cross-product-defect` / `fix-trades-sibling-gate`), none of them in code
this lane wrote; each one is a gate of a MERGED lane meeting another merged
lane's lift:

* `a_frame_edge_straddling_band_decodes_pixel_exact` (lane-cdef/golomb gate) --
  192x68 cq61 8-bit, frame 1 plane Y 6859 pixels differ, first at row 0 col 64
  (ours 56 vs ffmpeg 178), `edge32=[4,20,4,0,6,12,2,0]`. Green on main; the
  stream now decodes further because merged lanes lifted its earlier stop.
* `a_real_aomenc_inter_sequence_with_16x16_level_1to4_partitions_decodes_pixel_exact`
  (lane-intra14/r14 gate) -- firing assert: `split-tx-8x4` arm reads 0 in
  `[1,1,2,2,0]` and NO attempt refuses on rectangular residual coding any more,
  so the arm's documented blocker is gone and a zero arm is unexplained. The
  ledger already records this exact red for merging `a37a4bc`.
* `a_real_aomenc_stream_with_a_rectangular_compound_wedge_decodes_pixel_exact`
  (lane-inter16ab gate) -- every 8-bit cq50 attempt still stops at "a
  COMPOUND_WEDGE mask on a non-square inter block (rect wedge codebook
  unimplemented)", i.e. on this merged tree the refusal inter16ab lifted is
  reachable again through another lane's path.

Nothing was ignored or weakened to hide these; they are reported as RED.

## Residue
* fix-now (not this lane): the `-ss 0` frontier is now an inter SB-level AB
  partition (HORZ_A/HORZ_B/VERT_A/VERT_B at the 64 root). It is the single
  refusal standing between this tree and 33+ decoded frames of the real film,
  and it is unowned by the eight lanes merged here.
* deferred(the four refusals in the table above): the 64-level intra strip
  witness. Three independent measurements now say the same thing — aomenc never
  chooses the partition on synthetic or film-sourced input (r2, r3: ~6300
  partition symbols), and the film frames that DO contain it always refuse
  first, on this tree too.
* accepted: the ignored gate arms plus the tripwire.
