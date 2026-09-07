# lane-lossless2 — AV1 lossless INTER decode

Branch `lane-lossless2`, base `99823eef`. Head `f4c75f48`.

The refusal `"a lossless INTER frame (its chroma plane is still coded as one
transform per block here)"` is gone from `stream.rs` and from
`refusal_inventory.rs` (both the list and the refusal->gate table); its gate
`a_lossless_inter_frame_is_refused_by_name` is retired and replaced by the
decode-exact gate below (class `refusal-lifted-without-a-gate`).

## Rules ported (each with its libaom reference)
| rule | libaom | where |
|---|---|---|
| chroma of an INTER block is a raster of TX_4X4 units | `read_tx_size` (`av1/decoder/decodeframe.c`): `if (xd->lossless[..]) return TX_4X4;` for every plane | `decode::read_inter_chroma_lossless`, called from the whole-block chroma site and the 128 mu-chunk site of BOTH `decode_inter_block` arms (compound + single ref) |
| same for an INTRA block inside an inter frame | same | `decode::read_intra_chroma_lossless`, called from the intra tail's whole-block and mu-chunk chroma sites |
| unit order is PLANE-MAJOR inside each 64x64 mu chunk | `decode_token_recon_block`: `for (row..) for (col..) for (plane..) for (blk_row..) for (blk_col..)` | both helpers; `decode_block`'s multi-unit chroma loop was interleaved U,V per unit and is now plane-major (identical symbol order at one unit per plane per chunk, i.e. every non-lossless case) |
| `txb_skip` chroma context offset | `get_txb_ctx` (`av1/common/txb_common.h`): `ctx_offset = num_pels(plane_bsize) > num_pels(tx) ? 10 : 7`; our chroma tables carry the offset-10 rows at `+3` | `offset = (blk_w > 4 \|\| blk_h > 4).then_some(3)` in both helpers |
| per-unit entropy context, stamped as the unit is read | `av1_set_entropy_contexts` per transform block | `record_mi_chroma(cu_mi, 8, 8, plane, grid)` per unit; the block-level re-stamp is suppressed (`mu_chroma`) and `mu_chroma_units` now rebuilds 4x4 units with an 8 px span on a lossless frame |
| an 8x4/4x8 inter leaf is TWO 4x4 units | `read_block_tx_size`'s lossless early return | `decode_inter_sub8_rect2` leaf list |
| no `tx_type` symbol, DCT_DCT, WHT, TX_4X4 luma | (lane-lossless) | unchanged |
| deblock/CDEF/LR off | `coded_lossless`/`all_lossless` in the frame header parser | verified unchanged: `read_cdef_params` forces `bits = 0`, the lf levels and LR are header-gated; no decoder-side change was needed |
| a frame mixing lossless and lossy SEGMENTS | every rule above is per segment (`xd->lossless[segment_id]`) | still refused by name — see Deferred |

## Defects this lane found (all invisible to the previous lane's gate)
The lane-lossless gate's source had a FLAT chroma plane: every chroma unit
coded one `txb_skip` symbol against all-zero contexts, so the gate was blind
to unit order and to per-unit contexts (class `gate-blind-to-feature`). With
textured chroma the KEY gate fails at `99823eef`. Three root causes:

1. `decode_block` read chroma units interleaved U,V; libaom is plane-major.
2. `decode_rect_split`'s lossless chroma branch stamped each unit's context
   and then let `record_split_luma_rect_mi` overwrite all of them with the
   LAST unit's state — the next block read `dc_sign_ctx` 1 where libaom reads
   0 (class `override-slot-on-one-arm`). Localised with the aomdec oracle:
   `EC_TRACE_COEFF` on both sides, first divergence at chroma unit 717 of a
   256x128 `-crf 0` key frame, `dcctx=1` vs `dcctx=0`.
3. `decode_inter_sub8_rect2` kept ONE 8x4/4x8 luma transform on a lossless
   frame, feeding a rect grid to the 4x4 Walsh-Hadamard (panic, not garbage).

Our `EC_COEFF_STEP tag=all_zero` trace now prints `dcctx=` so it lines up with
aomdec's field.

## Fixtures and results
`ffmpeg -c:v libaom-av1 -crf 0 -b:v 0 -cpu-used 6`, synthetic source with
TEXTURED luma AND chroma and per-frame motion.

| fixture | gate | result |
|---|---|---|
| 256x128, 2 key frames, 8-bit | `a_lossless_libaom_key_frame_decodes_sample_exact` (source now textured chroma) | EXACT |
| 256x128, 8 frames `-g 4`, 8-bit | `a_lossless_libaom_inter_frame_decodes_sample_exact` | EXACT |
| 192x96, 6 frames `-g 3`, 10-bit | same gate | EXACT |
| 320x192, 6 frames `-g 3`, 8-bit | same gate | EXACT |
| 256x256, 4 frames `-g 2`, `-aom-params sb-size=128` | same gate | EXACT |

## Screen content (palette) — three more defects, found by a `tune-content=screen` stream
| defect | libaom | fix |
|---|---|---|
| a 4x4 chroma unit got the WHOLE-BLOCK palette prediction buffer and read it at its own stride, so every unit reconstructed the block's top-left corner | the luma split-transform paths already windowed; `predict_and_reconstruct_intra_block` predicts per unit | `decode::palette_window`, shared by `decode_block`, `decode_rect_split` and `read_intra_chroma_lossless`. 320x192 screen key frame: 9600/15360 chroma samples wrong -> 0 |
| the two-cell neighbour read ran past the band for a unit whose block hangs off the frame edge (libaom still codes that unit: `max_blocks_high` clips in LUMA mi and then ROUNDS UP for chroma) | `get_txb_ctx` reads exactly ONE chroma entropy entry for TX_4X4 (`a[0]`/`l[0]`) | both helpers read one cell (`around_mi(cu_mi, MI)`) -- our bands carry that entry replicated over the unit's two luma-mi cells, so it is the same answer and never runs off |
| a unit whose ORIGIN is past the frame edge was still read | `max_blocks_wide/high` | origin clip in both helpers |

`tune-content=screen -crf 0` KEY frames at 320x192 and 640x384: sample-exact
(`dump_yuv` vs `ffmpeg -pix_fmt yuv420p10le`, 0/122880 and 0/368640).

## Gate results
- `a_lossless_libaom_key_frame_decodes_sample_exact` + `a_lossless_libaom_inter_frame_decodes_sample_exact`: PASS (2 passed).
- `cargo check --workspace --all-targets -j4`: RC=0, 0 errors, 0 ec-av1 warnings (the 21 warnings are pre-existing `ec-opus`).
- DECIDING GATE `bd_rate_screen_native`: **RED**, one reference point:
  `LADDER DECODE FAILURE: libaom-av1 ["-cpu-used","6","-b:v","0","-crf","5"] frame 0 plane Y sample 625: our decoder decoded 41, ffmpeg 40`.
  That point used to be refused; it now decodes, and frame 0 (a KEY frame) is
  wrong in ONE luma sample at (col 625, row 0) of 1920x1024. The BD row itself
  computed: +33.4% vs libaom, -23.8% vs rav1e.

## Deferred
- `deferred: the bd_rate_screen_native crf-5 screen point — ONE luma sample (625, row 0) of frame 0, a lossless KEY frame, decodes 41 where ffmpeg decodes 40; every other sample of every other point is exact. Not reproduced by a synthetic 1920x1024 tune-content=screen -crf 5 key frame (0 samples differ), so it needs the gate's own stream — unblocks: dump the ladder stream (EC_ENC_OUT / the external_ladder recipe) and diff EC_TRACE_COEFF against aomdec around that block.`
- `deferred: lossless INTER frames of SCREEN content desync — reproduced at 640x384 tune-content=screen -crf 0 -g 3: key frames 0 and 3 are sample-exact, inter frames 1/2/4/5 are not. Localised with the aomdec oracle to all_zero unit #23422 of the stream: the first LUMA unit of a block right after a sub-8x8 group reads txb_skip_ctx 3 where libaom reads 1 (one neighbour magnitude > 3 where libaom sees 0) — unblocks: that one block's above-band luma level; the synthetic non-screen inter fixtures in the gate are all exact, so it is screen/sub-8x8 specific.`
- `deferred: a screen-content KEY-frame fixture is not yet IN the gate — verified by hand with dump_yuv this round; adding it to a_lossless_libaom_inter_frame_decodes_sample_exact is a one-case edit but the crate suite for this report was already running — unblocks: one suite re-run.`
- `deferred: a frame mixing lossless and lossy segments — still refused by name ("a frame mixing lossless and lossy segments (the TX_4X4/WHT rules are per segment there)"): every rule above keys off a frame-wide `lossless_flag`, so per-segment support means threading `segment_id` through `lossless()`, `lossless_tx()`, the WHT switch and the filter gates — unblocks: a libaom recipe that emits one. Four tried this round at -crf 0 (aom-params aq-mode=1, aq-mode=2, aq-mode=3, deltaq-mode=1): none produced a frame whose segments disagree — the refusal never fired, i.e. every segment stayed at qindex 0.`
