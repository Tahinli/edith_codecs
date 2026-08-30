# lane-part32 r1 report

## What landed

`crates/ec-av1/src/decode.rs`: the INTRA 32x32 dispatch (key-frame tile path,
the one that previously hit `"a 32x32 partition type this decoder does not
code (value={part32})"`) now handles `PARTITION_HORZ_A` (4), `PARTITION_HORZ_B`
(5), `PARTITION_VERT_A` (6), `PARTITION_VERT_B` (7) -- mirrors cd6cb6d's INTER
AB arms exactly, using `decode_block` (square 16x16, `TxbSet::Luma16`/
`Chroma8`) for the two square sub-blocks and `decode_block_rect` (32x16 or
16x32) for the strip. The catch-all refusal is unchanged text but now only
reached for `PARTITION_HORZ_4`/`PARTITION_VERT_4` (values 8/9) -- narrowed by
construction, not by editing the string (still pinned as-is in
`refusal_inventory.rs`, no change needed there since the text is identical).

Four new thread-local hit counters (`intra_horz_a_hits`/`intra_horz_b_hits`/
`intra_vert_a_hits`/`intra_vert_b_hits`), one per arm, mirroring
`PARTAB_HITS`'s pattern but split so a recipe that only ever fires one arm
can't hide behind a shared total.

## Gate

`crates/ec-av1/src/stream.rs::a_real_aomenc_intra_stream_with_ab_partitions_decodes_pixel_exact`
-- single key frame, 64x64, `mandelbrot` content (same recipe shape as the
existing `a_real_aomenc_intra_stream_with_tx_select_decodes_pixel_exact`),
`--min-partition-size=16 --max-partition-size=32` (same bound lane-partab used
to stay below the still-unlanded SB(64)-level AB territory), 40 attempts.
Forbids the four `value={4,5,6,7}` intra refusal strings once reached, hard-
asserts all four per-arm counters > 0.

**PASSES**, `EC_AV1_REQUIRE_AOMENC=1` included:

```
a_real_aomenc_intra_stream_with_ab_partitions_decodes_pixel_exact:
0 named refusals, 40 pixel-exact matches out of 40,
horz_a=34 horz_b=45 vert_a=15 vert_b=18
```

EVIDENCE: `cargo test -p ec-av1 --lib a_real_aomenc_intra_stream_with_ab_partitions_decodes_pixel_exact -- --nocapture`
(with and without `EC_AV1_REQUIRE_AOMENC=1`) | both runs above | 40/40 pixel-exact, all 4 counters nonzero.

## Suite: NOT fully green -- one PRE-EXISTING failure exposed, out of this
## lane's scope

`cargo test -p ec-av1 --lib`: **264 passed, 1 failed** (19 ignored), 208s.

Failure: `stream::tests::a_real_aomenc_stream_with_a_superblock_level_horz_vert_partition_decodes_pixel_exact`,
seed 42, first luma mismatch at (row=0, col=91), inside superblock (0,1)'s
`PARTITION_VERT`-at-64 arm (`decode_block_rect64`, lane-sbpart territory --
mode=DC_PRED, all_zero=1, off by one: got=80 want=81).

Root-cause isolation done this round:
- Confirmed with `git checkout 21a71ec -- decode.rs stream.rs` (temporary,
  reverted) that this exact seed/recipe/byte-for-byte stream **already
  refused** on main at `"a 32x32 partition type this decoder does not code
  (value=4)"` -- inside superblock (1,1), which decodes strictly *after*
  superblock (0,1) in tile order. Entropy decoding is causal: nothing this
  lane touched can change how SB(0,1) is decoded, since SB(0,1) is decoded
  and its pixels finalized before the stream ever reaches the part32 AB
  symbol this lane added.
- This is the "refusal hides a defect" class already in the ledger
  (`refusal-hides-a-defect.md`): the sbpart gate's `matched > 0` assertion
  only ever checked pixels on attempts that decoded *end to end*; every
  attempt that hit a downstream refusal (including this exact seed, before
  this lane) never had its earlier SB(0,1) pixels compared at all. Lifting
  the part32 refusal let decode run to completion for seed 42, and the
  frame-level pixel compare then found a pre-existing `decode_block_rect64`
  defect for the first time.
- Not investigated further (turn budget): whatever's wrong in the
  32x64/64x32 rect-64 corner-truncated intra path is `decode_block_rect64`'s
  own bug, unrelated to `decode_block`/`decode_block_rect` (this lane's
  functions) and to the AB dispatch. It belongs to lane-sbpart, not
  lane-part32.

Per this repo's own rule ("Landing red ON THE LANE is fine -- I do the merges
to main and I never merge red"), HEAD is committed red with this failure
named. **Do not merge without either (a) fixing `decode_block_rect64`'s
DC-prediction/edge bug (a lane-sbpart-shaped charter, not part32's) or (b) an
explicit call that this newly-exposed defect is acceptable to carry
forward.**

`cargo check --workspace --all-targets`: clean.

## Troy extract

Cut `fixtures/part32/troy-extract.obu` (gitignored) via
`ffmpeg -ss 600 -t 2 -c:v copy -an -f obu -bsf:v extract_extradata` from
`~/Videos/Films/Troy.Director's.Cut.2004.Bluray.1080P.AV1.OPUS.5.1-DECK.mkv`.
`cargo run -p ec-av1 --example decode_probe -- fixtures/part32/troy-extract.obu`:

```
REFUSED: unsupported: AV1 decode_stream (a stream whose bit depth is not 8
(this decoder reconstructs into 8-bit planes))
```

No longer stops at the part32 refusal (charter's predicted outcome: Troy is
10-bit and hits the bit-depth wall first, per `his-av1-library-is-10-bit.md`).

EVIDENCE: `cargo run -p ec-av1 --example decode_probe -- fixtures/part32/troy-extract.obu`
| ran against a real 2s Troy extract at ~10min mark | output above.

## HEAD

`69eeb6e` -- "wip(part32): intra 32x32 AB arms (HORZ_A/HORZ_B/VERT_A/VERT_B)
decode, gated 40/40 pixel-exact, all 4 hit counters fire"

## Open

- `HORZ_4`/`VERT_4` (values 8/9) not attempted this round; refusal still
  names them (unmodified string, now only reachable for those two values).
  Not reached by any gate recipe tried, reachability unknown.
- The `decode_block_rect64` DC-prediction/edge defect above is the actual
  blocker for a fully green suite. It is NOT part32's code and needs its own
  lane/charter (lane-sbpart shaped) to isolate and fix.
- `--cq-level=45`/`--enable-tx-size-search=0` etc kept identical to
  lane-partab's proven recipe; not swept for a lower-cq/higher-fidelity
  variant.
