# lane-intra64split r1 HANDOFF (tip 23282bc code + 71735b3 report + this file)

## Premise correction (MEASURED -- the charter's shape was wrong)
The refusal "a split intra strip whose transform unit is 32x64 / 64x32"
(decode.rs, `decode_rect_split`'s `luma_rect` match) does NOT fire on a tx-split
strip. `depth_to_tx_wh` can never return a 64-axis unit
(`sub_tx_size_map[TX_64X32] == TX_32X32`). The ONLY caller passing a 64-axis
tx is `decode_intra_rect_in_inter` (decode.rs:~6252) at **tx_depth 0**, where
`(tx_w,tx_h) == (bw,bh) == 64x32/32x64`. Its `depth != 0` arm refuses earlier by
lane-intrasplit's name ("a split (nonzero tx_depth) transform on an intra
HORZ/VERT strip in an inter frame") -- untouched here.

## Implemented (commit 23282bc)
* `decode.rs` counter `RECT64_CORNER_TU_HITS` + `rect64_corner_tu_hits()`;
  `stream.rs:74` public accessor.
* `luma_rect` match: `(64,32)|(32,64) => None` + `luma_64corner` flag; the `_`
  catch-all refusal and its `refusal_inventory.rs` line STAY (the match must be
  exhaustive; string is parametrised) -- deviation, reasoned in the report.
* New TU arm in `decode_rect_split`: `read_coeffs` on `TxbSet::Luma64`,
  `default_scan(TX32)`, txb_skip ctx 0 (unit covers its whole block,
  `get_txb_ctx`), `TxType::DctDct` (EXT_TX_SET_DCTONLY at square-up TX_64X64),
  `Some((tx_w,tx_h))` so `av1_nz_map_ctx_offset` uses the RAW rect shape, 32x32
  corner copied into the tx_w*tx_h grid, `dequant_and_inverse_typed_wh`,
  `reconstruct_rect`, `record_mi_luma_rect`. Mirrors `decode_block_rect64`'s
  depth-0 key-frame read (decode.rs:~8069). Chroma unchanged (ChromaRect32x16,
  32x16 -- never split with luma for intra).
* tx_depth read, per-TU raster recon, `Reach::of_tu`, palette slicing: all
  pre-existing `decode_rect_split` machinery, reused unmodified.

## Gate -- NO RESULT YET (the blocking residue)
`a_real_aomenc_inter_sequence_with_a_64_level_intra_rect_strip_decodes_pixel_exact`
(+ `_10bit`), stream.rs:~4275. 256x192 (whole superblocks -- the 96x96 sibling
gate can never offer a 64-level partition), mandelbrot fast zoom,
`--enable-rect-partitions=1 --min-partition-size=32 --max-partition-size=64
--enable-tx-size-search=0` (charter's `=1` stops at lane-intrasplit's refusal
before the shape), 40 attempts x cpu-used 0..4 x cq 30/20/12/45, all planes,
every decode-order frame, hard `rect64_corner_tu_hits > 0`.
TIMING: the standalone run (both depths, `--nocapture`) ran **65 min without
finishing** and was killed by the harness with its output piped through
`grep|tail`, so ZERO lines survived. The recipe is too slow: cpu-used=0 at
256x192x8 frames dominates. NEXT: cut to cpu-used in {2,3,4}, 12-16 attempts,
frame_count 6, and run detached writing straight to a file
(`systemd-run --user --unit=... > log`), never through a pipe.
Also UNPROVEN: whether aomenc emits a 64-level intra strip in an INTER frame at
all at this size (the sibling gate's r2 note says it could not at 96x96). The
film proves the shape is real -- if attempts miss, widen frame/content, never
weaken the assert.

## Suite
Unit `intra64split-suite-1788329481.service`, log `$HOME/.cache/intra64split-suite-r1.log`
-- STILL RUNNING at handoff (1261 lines, no `test result` line yet; my own slow
gate is inside it). STOP it before starting another suite on the same target dir
(`systemctl --user stop intra64split-suite-1788329481.service`).

## Film probe (4K 10-bit AV1 mkv, `-ss 300 -t 2`, obu at
/tmp/claude-1000/-home-tahinli-Documents-Code-Rust-edith-codecs/b6d8a07f-96a4-4bbb-b378-af9ae25cf7c9/scratchpad/i64/hg300.obu)
BEFORE (census4 hunger4.tsv, start_s 300 and 3000): "a split intra strip whose
transform unit is 32x64".
AFTER: 206 frame headers parsed, stops at "a split transform on a 1:4 inter
strip with a 64-px axis"; counters `rect4_32: horz=104 vert=172 coded=238`,
`inter_rect: 32x64=2`. Pixel-exactness of the new arm on the film is NOT proven
(probe only decodes).

## Exact next step
1. Trim the gate recipe as above; run it detached to a file; read the per-attempt
   `intra64split gate ...: 64-level unsplit intra strip TUs=N` lines.
2. If TUs>0 and pixels match -> GREEN, then run the suite to N/0 and merge.
   If pixels mismatch -> the suspects, in order: `Reach::of_rect` at the strip
   (top-right availability of a 64-wide strip), the `Some((tx_w,tx_h))` raw-shape
   ctx offset, and `record_mi_luma_rect` over a 64-axis span.
