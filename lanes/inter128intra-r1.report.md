# lane-inter128intra r1 -- the 128-root HORZ/VERT half coded INTRA in an inter frame

Branch `lane-inter128intra` (base main `5dd3dcb`), commit `5c114d5`,
worktree `edith_codecs-inter128intra`,
`CARGO_TARGET_DIR=$HOME/.cache/cargo-target-inter128intra`.

## Verdict up front

The arm is IMPLEMENTED but **not gated and therefore not lifted**: it decodes
only under `EC_INTRA128_IN_INTER=1`, the named refusal is untouched on the
default path, `refusal_inventory.rs` / `gate_coverage.rs` are unchanged. Two
findings force that:

1. **The charter's film premise is a downstream artifact** (class
   `refusal-from-own-desync`): on `t900` the 128x64 intra arrival at
   mi(128,416) sits *after* a pre-existing entropy desync in the same frame.
2. **No aomenc recipe found emits the shape**: 3 recipes x 4 cq values produced
   ZERO 128-root HORZ/VERT halves in an inter frame (`inter_128x64=0` on the
   decoder side), so there is nothing to gate on yet.

## Measurement (step 1, before any edit)

`EC_OBMCREC=1 decode_probe` on the three 2 s cuts of the 10-bit 1920x792
128-superblock stream:

| cut | shape | mi | skip |
|---|---|---|---|
| t900 | 128x64 | (128,416) | false |
| t5400 | 128x64 | (192,0) | false |
| t8100 | 128x64 | (192,0) | false |

EVIDENCE: ~/.cache/rectres-tmp/{t900,t5400,t8100}.obu | EC_OBMCREC=1 decode_probe per cut | 3/3 arrive as a non-skip 128x64, premise shape confirmed

## Cross-decoder ladder -- the premise is stale

`t900` truncated to its first 2 frames (`census6/trunc.py`,
`~/.cache/inter128intra-tmp/t900_2.obu`, 85285 B), ours
`EC_TRACE_MODE_STEP=1` vs `~/.cache/aom-oracle/build/aomdec`
`EC_TRACE_MODE=1 EC_TRACE_MODE_STEP=1`:

- **Key frame exact**: the ordered `EC_IMODE mi_row/mi_col` sequence is
  identical for all 2704 blocks (ours has 184 extra lines, all from frame 2's
  intra-in-inter blocks, which aomdec does not print at all).
- **Frame 2 last common symbol**: `mi_row=24 mi_col=312 name=tx_depth rng=38975`
  -- byte-identical in both traces.
- **First divergence**: the very next superblock, mi(0,320) (pixel 1280,0).
  aomdec reads INTRA `tx_depth` for small blocks --
  `(0,320) val=1 ctx=0 cat=1 rng=54736`, `(0,322) rng=53468`, `(0,324)`,
  `(2,324)`, `(4,320)`, `(5,320)`… -- while ours reads var-tx
  `txfm_split_rect mi=(2,324) ctx=12 rng=53248` then
  `txfm_split mi=(4,324) ctx=13 rng=46998`, i.e. we take that block as INTER
  where libaom has intra. None of our three rng values (53248 / 46998 / 37590)
  occurs anywhere in the reference's frame-2 trace.
- mi(0,320) is decoded **hundreds of blocks before** mi(128,416) in the same
  frame, and main's build walks the same path (it refuses only later, at
  mi(128,416)) -- so the desync is PRE-EXISTING on main `5dd3dcb`, not caused by
  this lane, and the "128x64 intra in an inter frame" shape at that mi is what
  the desync produced.

EVIDENCE: ~/.cache/inter128intra-tmp/{ours.trace,aom.trace,o.f2,a.f2} | trunc t900 to 2 frames, EC_TRACE_MODE_STEP ours vs instrumented aomdec | key frame 2704/2704 blocks identical; frame 2 diverges at SB mi(0,320), last common rng=38975 at mi(24,312)

## What changed (all inert unless `EC_INTRA128_IN_INTER=1`)

- `crates/ec-av1/src/decode.rs:7389` `decode_intra_rect_in_inter`: new
  `bw.max(bh) == 128` arm -- sets `INTRA_IN_INTER_MODE = Some((3, skip))`
  (`size_group_lookup` common_data.h:61 puts BLOCK_128X64/64X128/128X128 in
  group 3, verified in the oracle tree) and calls the key frame's own
  `decode_block_128rect`, whose `tx_size_cat3` row is already the category a
  128-axis block reads (`max_txsize_rect_lookup` caps at TX_64X64).
- `crates/ec-av1/src/decode.rs:11609` `decode_block_128rect`: the `tx_depth`
  context comes from `tx_size_context_txfm_rect(.., bw.min(64), bh.min(64))`
  when the block is intra-in-inter (libaom `get_tx_size_context` compares the
  TXFM_CONTEXT bands against `tx_size_wide/high[max_txsize_rect_lookup[bsize]]`
  = 64 here), and the block publishes `set_txfm_ctxs` over its own footprint
  (`txfm_partition_update_rect`) -- the two inter-frame differences the key
  frame does not have. Key-frame behaviour is untouched (`in_inter == false`).
- `crates/ec-av1/src/decode.rs:11519` `INTRA128_IN_INTER_HITS` +
  `crates/ec-av1/src/stream.rs:156` `intra128_in_inter_counters()` +
  `crates/ec-av1/examples/decode_probe.rs:81` prints
  `intra128_in_inter: 128x64=.. 64x128=..`.

## Recipe sweep -- aomenc never picked the shape

`--sb-size=128 --min-partition-size=64 --enable-rect-partitions=1
--kf-max-dist=100 --end-usage=q --cpu-used=0 --lag-in-frames=0 --limit=6`
over 256x256 8-bit sources:

| source | cq | frames | intra128_in_inter | inter_128x64 |
|---|---|---|---|---|
| full-frame temporal noise (`noise=alls=90:allf=t+u:all_seed=43`) | 32 | 6 OK | 0 | 0 |
| same | 55 | 6 OK | 0 | 0 |
| static gray + two 256x64 noise bands at y=64,192 (forces a HORZ split with one intra half) | 20 | 6 OK | 0 | 0 |
| same | 45 | 6 OK | 0 | 0 |

The decoder-side `inter_128x64` counter is 0 in every arm: aomenc did not
choose the 128-root HORZ/VERT partition in an inter frame at all, so this is a
partition-choice problem, not an intra/inter one -- a gate needs either a
recipe that produces the partition first, or the film-prefix fixture route
(blocked by the desync above).

EVIDENCE: ~/.cache/inter128intra-tmp/{g_32,g_55,h_20,h_45}.obu | aomenc 4 arms + decode_probe | 4/4 decode OK 6 frames, 128-root rect partitions 0/4

## Film cuts (10-bit 1920x792 128-SB, 2 s each)

| cut | default (refusal untouched) | with `EC_INTRA128_IN_INTER=1` |
|---|---|---|
| t900 | intra-coded 128x64 block on the inter block path | Golomb tail; 128x64=0 (desync kills it inside the first such block) |
| t5400 | intra-coded 128x64 block on the inter block path | Golomb tail; 128x64=453 64x128=20 |
| t6300 | Golomb tail (unchanged from main) | Golomb tail; 0 |
| t8100 | intra-coded 128x64 block on the inter block path | Golomb tail; 128x64=30 |

The opt-in numbers are NOT a capability claim: every one of those blocks is
downstream of the same class of upstream desync proven for t900, and the
"Golomb tail" wall is itself a desync symptom (`refusal-hides-a-defect`).

EVIDENCE: ~/.cache/rectres-tmp/{t900,t5400,t6300,t8100}.obu | decode_probe with and without EC_INTRA128_IN_INTER=1 | default stop strings identical to main; opt-in advances past the shape into a Golomb wall

## Suite

`cargo test -p ec-av1 --lib` under systemd unit
`inter128intra-suite-r1-1788367879` (MemoryMax=10G),
log `$HOME/.cache/inter128intra-suite.log`: SUITE_TOTALS_PLACEHOLDER

## Residue

- fix-now (own lane, blocks this one): the frame-2 desync at SB mi(0,320) of
  the 2 s cut at 900 s -- last common symbol mi(24,312) `tx_depth` rng=38975,
  first divergence one superblock later where aomdec has intra blocks with
  `cat=1` at (0,320)/(0,322)/(0,324)/(2,324) and ours reads var-tx
  `txfm_split_rect` at (2,324). Truncated 2-frame witness pinned at
  `~/.cache/inter128intra-tmp/t900_2.obu`; both traces kept next to it.
- deferred: the gate for this arm -- needs a recipe that makes aomenc pick a
  128-root HORZ/VERT partition in an INTER frame (0/4 attempts here), or a film
  prefix whose counter is > 0 with the stream proven exact up to it (blocked by
  the desync above).
- accepted: `128x128` NONE intra-in-inter was not swept -- it never reaches
  `decode_intra_rect_in_inter` (that path is entered only when
  `write_w != side`), so it is a different call site with its own residue.
