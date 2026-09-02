# lane-frame36 r2 report (branch `lane-frame36`)

Tip: `c9e15d7` (r1 tip was `fa20907`).

## Root cause found this round (one, entropy-level)

`decode_block_rect64`'s `depth != 0` arm returns early, past the tail
`txfm_partition_update_rect` that r1 added -- so a SPLIT superblock-level strip
(64x16 / 16x64 / 64x32 / 32x64) published NO `TXFM_CONTEXT` band, and the next
block read the row's init value 64 as its left transform height. libaom runs
`set_txfm_ctxs(mbmi->tx_size, ...)` for every block, with the SPLIT unit's size.

Measured (decode-order frame 33, the hidden ARF, of the 58-OBU prefix): the
16x64 strip at mi(192,140) reads `tx_depth=1` -> TX_16X32, so libaom's
`left_txfm_context[192]` is 32; ours stayed 64, and the 32x64 intra block at
mi(192,144) read `tx_size_cat3` row 2 where aomdec reads row 1 -- same value,
different range (class `wrong-alphabet-same-value`), desync to the end of the
frame. Fix: `crates/ec-av1/src/decode.rs:9594` (one call), plus the counter
`rect64_split_txfm_publish_hits` and the oracle-format
`EC_ISTEP ... name=tx_depth ... cat=` print in `decode_intra_rect_in_inter`
(decode.rs:7541) that named it.

## Class sweep (same shape)

`decode_block_rect4` (32-level 1:4 strips; also the inter frame's reader for an
intra 32x8/8x32) published the bands on NONE of its three exits -- fixed the
same way (decode.rs:8625/8776/8839). Latent here: once frame 33 decodes
correctly this stream carries no intra 32x8/8x32 at all (the 48/40 counted
before the fix were phantoms of the desync, class `counter-from-refused-stream`).
The 48-frame decode was re-measured byte-exact WITH the sweep in.

## Frame 33 state

EXACT. Every superblock ENTRY range of every traced frame of the prefix equals
aomdec's (39 groups ours vs 40 aomdec: our SB-root print skips the last partial
SB row, and our group 0 holds frames 0+1; the mapping is ours[i] = aomdec[i+1]).
No PREFILT16 stage compare was needed -- the pixels are equal.

## Film (the 2 s head cut of the 10-bit 3840x1608 stream)

`~/.cache/hg-0.obu`, 48 decode-order frames: differing bytes per frame
`0,0,0,...,0` (all 48 zero) vs `ffmpeg -pix_fmt yuv420p10le`. The r1 wall
(output frame 36, 765464 differing bytes; 1992768 before r1) is CLOSED.

## Gate

`a_10bit_film_hidden_arf_with_split_superblock_strips_decodes_pixel_exact`
(`crates/ec-av1/src/stream.rs`), fixture `crates/ec-av1/fixtures/hg_arf_witness.obu`
(32126 B, sha256 7f3b060da5aa9c537633e760c4af71316237db7692f6ae0d459bd2c2dd867d37,
`git add -f`): every plane of all 37 shown frames vs ffmpeg, hard assert
`rect64_split_txfm_publish_hits > 0` (4 on this stream).
Command:
`EC_NOMEMGUARD=1 EC_AV1_REQUIRE_AOMENC=1 CARGO_TARGET_DIR=$HOME/.cache/cargo-target-frame36 cargo test -p ec-av1 --lib a_10bit_film_hidden_arf_with_split_superblock_strips -- --nocapture`
Result: `ok. 1 passed`, 94.77 s.

## Refusals

None lifted this round (the defect was a wrong decode, not a refusal), so
`refusal_inventory.rs` is unchanged; `gate_coverage.rs` needs no entry either --
this gate runs a pinned film stream, not an `aomenc` recipe.

## EVIDENCE

EVIDENCE: ~/.cache/frame36-tmp/r2/{omode5,amode}.txt | ours EC_TRACE_PART+EC_TRACE_MODE+EC_TRACE_MODE_STEP vs aomdec EC_TRACE+EC_TRACE_MODE+EC_TRACE_MODE_STEP+EC_VARTX on the 58-OBU prefix, SB-root range compare per frame | 0 differing superblock entry ranges in every frame (was: first diff at SB(192,160) of frame 33, 38758 vs 39618)
EVIDENCE: ~/.cache/frame36-tmp/{o2.raw,r.raw} | decode_probe EC_PROBE_OUT16 vs ffmpeg -pix_fmt yuv420p10le on the 58-OBU prefix, diff16.py 3840x1608 | all 37 output frames 0 differing bytes (frame 36 was 765464)
EVIDENCE: ~/.cache/frame36-tmp/{full_o2.raw,full_r.raw} | same, on the whole 48-frame 2 s head cut | all 48 frames 0 differing bytes
EVIDENCE: ~/.cache/frame36-tmp/r2/omode4.txt | ours EC_TXCTX at mi(192,144) | above_txfm=64 left_txfm=64 -> ctx 2, aomdec ctx 1: the stale left band

## Residue

- accepted: our `EC_TRACE_PART` SB-root print skips the last partial SB row
  (tracing only; pixels of those rows are compared and exact).
- deferred(a longer film cut): only the 2 s head is proven; the next wall in the
  full film is unmeasured -- the census under `lanes/` is the unblocker.
