# lane-golomb r7 REPORT -- GREEN (the r6 chroma residue is closed at its root cause)

## What changed
* `crates/ec-av1/src/decode.rs` -- new `fn reduce_inherited_chroma_tx_type(t, w, h)`
  (inserted just above `fn inter_txbset_for`), and both inter-chroma callers now use it:
  * `read_inter_plane_rect`'s `default_tx_type` (was `Some(t) if w.max(h) < 32 => t`)
  * `read_inter_plane`'s `default_tx_type` (was `Some(t) if side < 32 => t`)
  Those two are the ONLY sites in the crate that derive a chroma `tx_type` for an INTER
  block (`rg inherited_luma_tx_type`); the intra chroma sites (`decode.rs:6056`, `9195`)
  take `default_intra_tx_type` and are already correct -- every value that table can
  produce is a member of both intra sets below 32, and both already force `DCT_DCT` at
  `>= 32`, which for INTRA is right (`av1_get_ext_tx_set_type` returns `EXT_TX_SET_DCTONLY`
  for intra at `tx_size_sqr_up == TX_32X32`).
* `crates/ec-av1/src/decode.rs` -- new unit test
  `decode::tests::an_inter_chroma_transform_narrows_its_inherited_tx_type_to_its_own_set`.
* `crates/ec-av1/src/stream.rs` -- comment only: the measured reason the 192x68 / 68x192
  straddling arms are NOT in `edge32_gate` (below).

## ROOT CAUSE (libaom citation)
`av1_get_tx_type` (`av1/common/blockd.h:1278-1309`) for a chroma plane of an INTER block:
1. `if (txsize_sqr_up_map[tx_size] > TX_32X32) return DCT_DCT;`  -- **strictly greater**;
2. `tx_type = xd->tx_type_map[(blk_row << ss_y) * stride + (blk_col << ss_x)]` -- the
   colocated luma unit's coded type (for a chroma unit at (0,0), the FIRST var-tx leaf);
3. `if (!av1_ext_tx_used[av1_get_ext_tx_set_type(tx_size, 1, reduced)][tx_type])
   tx_type = DCT_DCT;` -- narrowed by the set the **chroma** size selects
   (`av1_ext_tx_used` `blockd.h:1036`, `av1_get_ext_tx_set_type` `blockd.h:1097`).

We had step 1 as `chroma side >= 32 -> DCT_DCT` and no step 3 at all. At exactly
`TX_32X32` an inter block reads `EXT_TX_SET_DCT_IDTX`, which **contains IDTX**. So a
64x64 inter superblock, whose chroma units are 32x32 (`av1_get_max_uv_txsize(BLOCK_64X64)`),
whose first luma var-tx leaf coded `IDTX`, must inverse-transform its chroma with IDTX --
we used DCT_DCT.

`IDTX` and `DCT_DCT` are both `TX_CLASS_2D`: same scan, same coefficient contexts, same
msac ranges. That is why the whole stream stayed in entropy sync and only one superblock
of chroma came out as high-frequency noise with luma bit-exact -- the r6 handoff's exact
signature. (Class for the ledger: a tx_type set-membership bug is invisible to every
range ladder; only pixels see it.)

The old rule was also wrong the other way for chroma 16x16 under 8x8 luma leaves
(`EXT_TX_SET_DTT9_IDTX_1DDCT` excludes V_ADST/H_ADST/V_FLIPADST/H_FLIPADST, which we would
have inherited verbatim). The new helper covers all four sets.

## Localization (how the tx_type was identified, no aomdec rebuild)
`EC_TRACE_MODE=1 EC_TRACE_COEFF=1` on `g35.obu` (md5 `9037f5b21db95d35e71f91f040fc33e1`,
`scratchpad/gl-gen.sh 35`), split into frames on `EC_MODE mi_row=0 mi_col=0`: decode-order
picture 4 is segment 4. Its block at `mi(0,16)` (luma x=64,y=0) runs to the next
`EC_MODE mi_row=0 mi_col=32`, i.e. it is a **64x64** block (not 32x32 as r6 recorded),
var-tx split into 16x16 leaves. Its last two coefficient units are
`eob=1022 tx=DctDct` and `eob=1020 tx=DctDct` with `ctx=0` -- eob > 256 proves 32x32
chroma units -- while its FIRST luma leaf is `eob=254 tx=Idtx`. libaom inherits that
`Idtx`; we produced `DctDct`.

## Gate
`cargo test -p ec-av1 --lib -- a_real_aomenc_stream_with_a_32x32_frame_edge_rect_partition --nocapture`
(under `systemd-run --user --unit=golomb-gate-r7-... -p MemoryMax=10G`,
`EC_AV1_REQUIRE_AOMENC=1`, `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-golomb`)

BEFORE (r6 handoff): `192x80 cq35 frames=5 10bit=false frame 3 plane U: 978 pixels differ,
first at row 0 col 32 (ours 33 vs ffmpeg 8) [edge32=[0,36,0,0,0,18,0,0]]`.
AFTER: `ok` -- 5 arms x cq {35,40,45,50,55,57,59,61}, 8-bit and 10-bit,
`38 pixel-exact attempts, 32-level edge bits [horz_or_vert=0 split=394], 64-level edge
bits [horz_or_vert=7 split=197] right-VERT=7, 2 named refusals` (both the inter SB-level
AB partition another lane owns).

EVIDENCE: $HOME/.cache/golomb-gate-r7.log | cargo test -- a_real_aomenc_stream_with_a_32x32_frame_edge_rect_partition --nocapture, 40 aomenc encodes decoded and compared plane-by-plane vs ffmpeg | 38/38 compared attempts pixel-exact, 0 differing pixels (was 978 U pixels on the 192x80 cq35 5-frame arm)
EVIDENCE: $HOME/.cache/golomb-gate-r7b.log | same gate + arms (192,68,5) and (68,192,5) | 68x192 cq35 frame 1 plane Y 141 px differ first at row 0 col 64 -- separate OPEN defect, arm not kept
EVIDENCE: scratchpad/r7-ours.err | EC_TRACE_MODE=1 EC_TRACE_COEFF=1 decode_probe g35.obu, segment 4, block mi(0,16) | first luma leaf tx=Idtx, the two 32x32 chroma units (eob 1022/1020) took tx=DctDct

Unit check: `cargo test -p ec-av1 --lib -- an_inter_chroma_transform_narrows` => `ok. 1 passed`.

## Refusals lifted
None this round -- this is a wrong-pixels fix, not a refusal lift. `refusal_inventory` /
`gate_coverage` untouched.

## The 192x68 / 68x192 straddling arm (r3 verifier ask) -- NOT ADDED, measured why
* `(192, 68, 5, false)`: all EIGHT cq attempts refuse by name, `a reference picture whose
  height does not match this frame's own true size` -> the arm compares nothing (vacuous,
  class `counter-from-refused-stream`).
* `(68, 192, 5, false)`: RED on a real separate defect --
  `68x192 cq35 frames=5 10bit=false frame 1 plane Y: 141 pixels differ, first at row 0
  col 64 (ours 167 vs ffmpeg 166) [edge32=[0,34,0,0,1,17,0,1]]`. LUMA, inside the 4-px
  straddling COLUMN; the straddling ROW twin (192x68) never gets that far.
  disposition: deferred(the right-edge straddling-column luma defect above, and the
  reference-size refusal) -- both are outside this lane's chroma tx_type cause.

## Test totals
`$HOME/.cache/golomb-suite-r7.log` (unit `golomb-suite-r7`) -- see the `test result:` line.
NOTE: `$HOME/.cache/golomb-suite-r6.log` never produced a `test result:` line: it was still
`active` (254/404 tests, stuck in the long superres/encoder tests) and holding
`CARGO_TARGET_DIR`, so it was stopped -- it was measuring the PRE-fix tree and is superseded
by r7. Every `test` line it did produce was `ok` or a documented `ignored`.

## Open / residue
* `main` has advanced to `85887c7` (we merged `18bf7dc`); this branch is NOT re-merged.
* the two defects the 68-px arms found (above), deferred.
