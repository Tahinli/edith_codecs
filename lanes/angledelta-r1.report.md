# lane-angledelta r1 -- the intra `tx_depth` context read its neighbour's inter flag one mi cell off

## Defect
On main 752e952 the gate
`a_real_aomenc_10bit_inter_sequence_with_a_directional_intra_block_with_angle_delta_decodes_pixel_exact`
(crates/ec-av1/src/stream.rs:6294) FAILED: seed 46, frame 4, Y (31,0) got 524 want 523, 3357/4096
samples differ, frames 5..7 whole-frame wrong. Green on cf38133 -- but there the gate returned on
seed 47 (seed 46 refused before reaching the defect). Bisect over the 5 merges: green at cf38133,
aba55b2, 61aa768, 662c285, 2bac468; RED at 5c35efa (lane-sbab). That merge lifted the SB-level
inter AB refusal, which is what lets seed 46 decode at all -- class
[[merge-cross-product-defect]] / [[refusal-hides-a-defect]]: the defect itself is older
(3c174b6), it was simply unreachable.

## Root cause (path:line)
`crates/ec-av1/src/decode.rs:12236` `tx_size_context_txfm` (libaom `get_tx_size_context`) read
`above_inter[mi_c / (SUB / MI)]` / `left_inter[mi_r / (SUB / MI)]`, i.e. the 16-px cell index,
while `Neighbours::record_inter_rect_mi` (decode.rs:4977) WRITES both bands per MI unit. For a
16x16 intra block at mi(4,4) in an inter frame the left neighbour is the inter block at mi(4,0),
but `left_inter[4/4] = left_inter[1]` names mi row 1 -- part of the intra block above -- so
libaom's "an inter neighbour contributes its BLOCK size, not its transform size" override never
fired. `left_txfm[4]` is 4 (that inter block's var-tx tree split to 4x4), so `left` read false and
the block took `tx_size_cat1` row 1 where aomdec takes row 2: same decoded value, different range,
entropy desync from that symbol on. Fix = index both bands by mi.

## Evidence
EVIDENCE: ~/.cache/angledelta-tmp/{ours_all.log,aom_all.log} | msac ladder of the pinned stream
s46.obu (md5 511efd772f3c0976c26fa2bf14ffb72d, hashed twice) ours EC_TRACE_COEFF+EC_TRACE_MODE(_STEP)
vs instrumented aomdec, diffed element by element | first divergence = frame 4 mi(4,4)
`tx_depth val=1`: ours ctx=1 rng=57990, aomdec ctx=2 cat=1 rng=48518; every element before it
(through the previous block's chroma all_zero rng=47098) identical.
EVIDENCE: ~/.cache/angledelta-tmp/txctx.log | temporary TXCTX print of the two bands at that read |
`mi=(4,4) left_txfm=4 li=false lsm=16` -- the inter override skipped; print removed before commit.
EVIDENCE: ~/.cache/angledelta-tmp/bisect.log | gate run at cf38133/aba55b2/61aa768/662c285/2bac468/
5c35efa in a detached worktree | ok,ok,ok,ok,ok,FAILED -- the lane-sbab merge is where seed 46 first
decodes.

## Gates (cargo test -p ec-av1 --lib -j3 <filter>, EC_AV1_REQUIRE_AOMENC=1)
- angle_delta: 2 passed (8-bit + 10-bit arms) -- was 1 failed.
- non_dc_chroma (uv8): 3 passed. intrabc: 1 passed. palette: 7 passed.
- gathered_edge_horz (sb128c): 1 passed.
- `a_frame_edge_straddling_band_decodes_pixel_exact`: still FAILED (red on main before this lane
  too; another lane owns it) -- disposition: deferred(the lane that owns that gate).

## Sweep (class [[context-read-from-one-cell]])
`grep -rn "mi_[rc] / (SUB / MI)"` over crates/ec-av1/src: the only remaining uses are the three
sites that convert to genuine SUB-granular arrays (`above_mode`/`left_side`, decode.rs 5634, 6928,
7895). No other MI-granular band is read with a cell index.

## Refusals
None lifted this round (pure entropy-context fix); refusal_inventory.rs / gate_coverage.rs
unchanged.

## Wider scoped run
`cargo test -p ec-av1 --lib -j3 inter`: 58 passed, 0 failed, 7 ignored (215s).
