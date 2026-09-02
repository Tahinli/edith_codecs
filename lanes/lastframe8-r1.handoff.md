# lane-lastframe8 r1 — handoff (root cause FOUND and FIXED, committed 9401108)

## Root cause (hypothesis (b): entropy desync inside the last frame)
Hypothesis (a) is DEAD: all 6 OBUs of the failing stream are ordinary
`show_frame=1, show_existing_frame=0` frames, refresh 0xff/0x02/0x04/0x08/0x10/0x20,
no no-show/overlay (decode_probe now prints this under `EC_PROBE_HDR=1`).

The defect is a **masked-compound CDF row read at the enclosing square `side`
instead of the block's true bsize**. A rect inter block is decoded by
`decode_inter_block` at `side = BLOCK` (an accepted corner-cut, decode.rs
PARTITION_HORZ/VERT comments), and `read_compound_type` picked its row with
`match side { 8=>3, 16=>6, 32=>9, _=>None }`. A 16x32 VERT half therefore read
`compound_type_cdf[BLOCK_32X32]` where libaom reads `[BLOCK_16X32]`: same decoded
value, different interval narrowing -> the msac range diverged inside the block
and the tile desynced at the NEXT block. Class:
price-the-narrowing-not-the-table + cdf-row-held-constant.

Fix (decode.rs): new `wedge_bsize_index(w, h)` keyed on `write_w`/`write_h`,
mirroring `av1_wedge_params_lookup`'s non-zero `wedge_types` rows
(3,4,5,6,7,8,9,18,19); the COMPOUND_WEDGE arm on a non-square block now refuses
by name ("a COMPOUND_WEDGE mask on a non-square inter block") because
`wedge::codebook` is square-only. That refusal never fired on the sweep.

## First diverging element (the measurement)
Stream `e_b_8_24_8.obu` (192x128, 8-bit, rect + 1:4, cdef on), decode-order
frame 5. Mode/mv ladder identical for 82 blocks, last good = mi(8,44), a 16x32
VERT half (EC_PART_VAL mi(8,40) bsize=9 value=2), NEAREST_NEARESTMV, ref (GOLDEN,
ALTREF), `comp_group_idx = 1` (masked compound) per a new instrumented aomdec rung
(`EC_ISTEP2 name=motion_mode|comp_group_idx|compound_idx|interp`, added to
`~/.cache/aom-oracle/src/av1/decoder/decodemv.c`, backup of the pre-edit file at
`~/.cache/lastframe8-tmp/decodemv.c.bak`).
Both decoders leave the mv read at rng=49012; aomdec then enters mi(16,0) at
**rng=45738**, this decoder at **rng=51104**, and decodes mode=13 ref0=1 single
where we decode mode=24 compound ref(1,6). Wrong-pixel region = the whole second
(last) superblock row, luma only (chroma is uninformative: the source is
`format=gray`), 8370 px, max |d| 87.

## cmp result after the fix
`e_b_8_24_8.obu`: `frames 6 len_o 6 len_r 6 bad [] nbad 0` (was `bad [(5, 8370, 87)]`).
Also exact: `e_b_8_24_4.obu`, `e_a_8_18_8.obu`, `a8.obu`. Whole 32-arm sweep:
15/15 decodable arms pixel-exact on all 6 frames, 17 arms stop at pre-existing
1:4 / SB-AB refusals (unchanged by this commit).
Post-deblock dumps (`EC_AV1_POSTDEBLOCK_DUMP`, ours vs aomdec) were 0-byte-diff on
frames 0-4 and 8370 on frame 5 before the fix.

## Open residue for the next round
`decode.rs:18772` has the SAME shape, unfixed: the interintra
`wedge_interintra`/`wedge_idx` row is `if side == 16 { 6 } else { 9 }` — the
square `side`, not `write_w`/`write_h`. It is reachable only when
`enable_interintra_compound` and the block is a rect strip decoded at side 16/32;
no sweep arm hit it. Same one-line cure: `wedge_bsize_index(write_w, write_h)`.
