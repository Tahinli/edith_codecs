# lane-rectwire r4/r5 report

VERDICT: FIXED — the rect coefficient decode is live and pixel-exact. The
free-partition gate reports `rect_coeff_hits=38` with 15 pixel-exact matches
and 0 mismatches, and `rect_coeff_hits > 0` is a hard assert again.

## Root cause
`decode_block_rect` read the tx-depth symbol unconditionally:

    let ctx = tx_size_context_rect(neighbours, (mi_r, mi_c), bw, bh);
    let depth = dec.symbol(&mut cdfs.tx_size_cat2[ctx]);

That symbol exists only under `TX_MODE_SELECT`. Both square paths gate their
`read_tx_size` on `tx_select` for exactly this reason; the rect path did not.
Several gate recipes pass `--enable-tx-size-search=0`, so the encoder wrote no
such symbol and the decoder consumed one that was never there — desyncing the
tile from that point on.

That is why both 32x16 strips of the pinned HORZ quadrant were wrong from their
very first pixel: not a subtle context bug, an off-by-one-symbol.

## The evidence that named it
r4's range ladder, read correctly, had already pinned it. Its numbers:
- entry into the mi(8,8) strip in sync — ours `rng=37856`, oracle `EC_IMODE
  rng=37856`;
- `skip`, `y_mode`, `uv_mode` identical values on both sides, our range after
  `uv_mode` = 58692;
- oracle's `EC_IMODE_VAL`, printed AFTER its own tx read, = 58692;
- ours after our tx read = 43570.

r4 read that as "our tx CDF row or context must be wrong" and refused to land
on an unconfirmed suspect, which was the right call on the evidence it had. But
the oracle's range being UNCHANGED across its whole block is the tell: the
oracle read no tx symbol at all. Equal ranges either side of a symbol means the
symbol was never consumed — that is a cheaper conclusion than a near-certain
CDF, and it needs no further instrumentation.

## Before / after
- r3/r4 posture: non-skip rect strips refused by name; 29 refusals, 11 matches,
  `rect_coeff_hits=0`.
- now: 25 named refusals, 15 pixel-exact matches of 40,
  `rect_partition_hits=75`, `rect_coeff_hits=38`, 0 mismatches.
- full lib suite green in this worktree.

Remaining refusals in that gate are other named gaps: rect strips in
screen-content frames (palette syntax is consumed for square blocks only), the
inter-path non-skip rect strip, and partition types this decoder does not code.

## Class
`symbol-consumption-gap`, and the reading rule is worth keeping: when a range
ladder shows the reference's range UNCHANGED across a region where ours moves,
suspect a symbol we read and it did not, before suspecting our tables.
