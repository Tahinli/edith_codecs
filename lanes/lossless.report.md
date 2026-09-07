# lane-lossless — AV1 lossless (qindex 0) decode

Branch `lane-lossless`, base `7ba28bcd`. Status: KEY-frame half SHIPPED, inter half refused by name.

## Rules implemented (each with its libaom reference)
| rule | libaom | where |
|---|---|---|
| TX_4X4 on every plane, no `tx_depth`/`txfm_split` symbol | `read_tx_size` (`av1/decoder/decodeframe.c`): `if (xd->lossless[..]) return TX_4X4;` before `max_txsize_rect_lookup` | `decode::lossless_tx`, `decode_block`, `decode_leaf8`, `depth_to_tx_wh`, `read_block_tx_size`(+`_rect`), `read_inter_luma8` |
| Walsh-Hadamard inverse instead of DCT/ADST | `inv_txfm_add` (`av1/common/idct.c`): `if (txfm_param->lossless) { assert(tx_type == DCT_DCT); av1_iwht4x4_add(...); }`; kernel `av1_highbd_iwht4x4_16_add_c` (`av1/common/av1_inv_txfm2d.c:20`), inputs `>> UNIT_QUANT_SHIFT` (2) | `transform::inverse_wht4x4` / `dequant_and_inverse_wht4x4`, `TxParams::lossless` |
| no `tx_type` symbol | `read_tx_type` (`av1/decoder/decodetxb.c`): "No need to read transform type for lossless mode(qindex==0)" | `read_plane`/`read_inter_plane`/`read_inter_plane_rect`: `coding.tx_type = None` |
| DCT_DCT on every plane (so the default scan) | `av1_get_tx_type` (`av1/common/blockd.h`): lossless returns DCT_DCT | same three readers' `default_tx_type` |
| `is_cfl_allowed` narrows to a BLOCK_4X4 chroma plane block (luma <= 8x8 at 4:2:0) | `is_cfl_allowed` (`av1/common/blockd.h`) | `decode::cfl_allowed_px`, used by every intra mode reader |
| chroma coded as 4x4 units, plane-major, with `get_txb_ctx`'s `+10` rows | `decode_token_recon_block` mu-chunk loop; `av1_default_txb_skip_cdfs[0][TX_4X4][10..13]` = 9961/30242/32117 (neutral 16384 in every other q-context) | `decode_rect_split` lossless chroma branch; `cdf_state::txb_skip_chroma_4` widened 3 -> 6 rows |
| loop filter / CDEF / LR off | header-level (`coded_lossless`/`all_lossless`), already handled by the frame-header parser | unchanged |
| a frame mixing lossless and lossy segments | every rule above is per segment | refused by name |

WHT orientation is pinned by MEASUREMENT: libaom's column-then-row-with-transpose
loop equals rows-then-columns in place on a raster grid; the other orientation
reconstructs sample 0 and DC-only units correctly and drifts +-1 elsewhere.

## Fixtures and results (all `ffmpeg -c:v libaom-av1 -crf 0 -b:v 0`, synthetic source)
| fixture | result |
|---|---|
| 64x64, 1 key frame, 8-bit | EXACT vs ffmpeg (0/6144 samples differ); 6488 entropy steps identical to aomdec `EC_TRACE_COEFF` |
| 256x128, 2 key frames (`-g 1`), 8-bit | EXACT both frames — pinned as `stream::tests::a_lossless_libaom_key_frame_decodes_sample_exact` |
| 192x96, 2 key frames, 10-bit | EXACT both frames |
| 128x64, 4 frames (`-g 4`, inter) | refused by name — pinned as `stream::tests::a_lossless_inter_frame_is_refused_by_name` |

## Deferred
- `deferred: lossless INTER frames — an inter block's chroma plane is still read as one transform per block (decode_inter_block's two chroma sites + the 128 mu-chunk loop); its luma leaves are already synthesized as TX_4X4 — unblocks: port the 4x4 chroma unit walk (the shape decode_rect_split's lossless branch already has) into decode_inter_block/decode_inter_block8/the sub8 readers.`
- `deferred: the 12-frame bd_rate_screen_native gate's libaom crf-5 point — that stream's inter frames now hit the named refusal instead of decoding, so the deciding gate cannot be green this round — unblocks: the inter chroma walk above.`
- `deferred: mixed lossless/lossy segments — refused by name; no libaom recipe emits one — unblocks: a segmentation fixture with per-segment qindex.`
- `deferred: 4:4:4 / monochrome lossless — the crate is 4:2:0 only.`
