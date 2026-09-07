# lane-lossless2 — AV1 lossless INTER decode

Branch `lane-lossless2`, base `99823eef`. Head `9fab5e37`.

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

## Deferred
- `deferred: a frame mixing lossless and lossy segments — still refused by name ("a frame mixing lossless and lossy segments (the TX_4X4/WHT rules are per segment there)"): every rule above keys off a frame-wide `lossless_flag`, so per-segment support means threading `segment_id` through `lossless()`, `lossless_tx()`, the WHT switch and the filter gates — unblocks: a libaom recipe that emits one (see below).`
