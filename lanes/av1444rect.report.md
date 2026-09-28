# lane-av1444rect — H1: 4:4:4 lossy + a 1:4 (RECT) partition

**Tree.** `lane-av1444rect` off `4155c7c7`, worktree `~/.cache/wt/av1444rect`.
Fix commit `f92776ba`. One decode.rs hunk, one gate, one pinned fixture.

## 1. Root cause

`decode_inter_block`'s `around_c` — the gather that decides a 1:4 inter
strip's chroma **coefficient context** — chose the 4:2:0 PAIR extent with no
`ss` gate:

```rust
// crates/ec-av1/src/decode.rs, decode_inter_block
let around_c = match strip_chroma {
    Some(s) if s.has_chroma => {
        let (pw, ph) = if s.horz {
            if ss_x(fctx) == 1 && ss_y(fctx) == 0 { (16, 4) } else { (16, 8) }
        } else { (8, 16) };
        ...
        neighbours.around_mi_rect(s.pair_mi, pw, ph)
    }
    _ => around,
};
```

At ss (0,0) no pair exists. `is_chroma_reference` (av1_common_int.h:1454):

```c
int ref_pos = ((mi_row & 0x01) || !(bh & 0x01) || !subsampling_y) &&
              ((mi_col & 0x01) || !(bw & 0x01) || !subsampling_x);
```

At 4:4:4 `!subsampling_y` and `!subsampling_x` are both true, so `ref_pos`
is **unconditionally true**: every block is its own chroma reference. A
HORZ_4 16x4 strip codes its OWN 16x4 chroma (the `strip_chroma` setter at
`decode.rs:50840` already gives it `pair_mi == at_mi` for exactly this
reason), and its context extent is the strip's own 4 px.

The pair gather used `ph = 8`, which makes `around_mi_rect` walk
`left[mi_r]` **and** `left[mi_r + 1]`. The strip's own 4-px extent walks
`left[mi_r]` only. So the U unit's `get_txb_ctx` OR-ed in a left chroma
neighbour that does not exist, `txb_skip_ctx` came out 1 where libaom's is
offset-7 row 7, and the arithmetic coder forked.

**The fix** is one match arm, gated on the same `ss` the plane geometry is:

```rust
Some(_) if ss_x(fctx) == 0 => { hit!(RECT4_INTER_OWN_CHROMA444_HITS); around }
```

`around` is the block's own `around_mi_rect(at_mi, write_w, write_h)`, which
at ss 0 IS the chroma block's extent. The horz `(16, 4)` sub-branch also
loses its now-redundant `ss_x == 1 &&` conjunct (it is unreachable at
ss_x 0, where the new arm has already returned).

**Class: `ss-gate-missing-on-a-4:2:0-pair-geometry`.** A 4:2:0 pair extent
(or anchor) hardcoded on a path that also runs at ss (0,0), where
`is_chroma_reference` makes the pair non-existent. The 4:2:0 reduction of
the expression is the tell.

### Class sweep (same batch)

| site | verdict |
|---|---|
| `decode_inter_block` `around_c` | **the defect** — fixed |
| `decode_rect4_16_strip` (intra key-frame 1:4), `decode.rs:17308` | already ss-gated (`if ss_x == 0 && ss_y == 0 { (lmi, bw, bh) }`, the `lane-av1-444` row) |
| `decode_rect4_16_intrabc` (`decode.rs:16466`) | **same class, still open** — see §6 |
| `read_inter_rect_chroma`, `decode_block_rect4/64`, the 128-root mu-chunk arm | already ss-parameterised (`uw << ss_x`, `chunk_chroma_w = 64 >> ss_x`) |

## 2. Localization, measured

**Fixture.** `aomenc --codec=av1 --profile=1 --passes=1 --end-usage=q
--cq-level=20 --cpu-used=2 --threads=1 --row-mt=0 --lag-in-frames=0
--kf-max-dist=100 --limit=2` over `ffmpeg -f lavfi -i
"testsrc2=size=128x96:rate=25" -frames:v 2 -pix_fmt yuv444p -f
yuv4mpegpipe`.

6441 B, sha256 `77f727e76d81fe48cd03709f799131677c04869b163e18d78e96bc742ec69153`,
FNV-1a64 `0x04368b2d2e9172b0`. Pinned at
`crates/ec-av1/fixtures/444_lossy_rect4_inter_witness.obu`.

`rate=25` is load-bearing and the sweep report did not say so: `testsrc2`'s
pattern is time-parameterised, so `rate=1` yields a different 8121-byte
stream that decodes exactly. The report's 6441 B / 8884-wrong / Y(91,32)
numbers reproduce only at `rate=25`.

**Red before.** frame 0 exact; frame 1: 8884 of 36864 samples wrong, first at
Y(91,32).

**Symbol fork.** `EC_SYMR` both sides, aligned 1:1 on
`(nsymbs, symbol, post_rng, 32768 - our_icdf0)`: reads 0..26262 pair
exactly, read **26263** diverges, both sides at mi (8,16):

| | site | read |
|---|---|---|
| oracle | `decodetxb.c:158` (the `txb_skip` read) | plane 1 (U), `txb_skip_ctx` **7**, symbol 1 (all-zero) |
| ours | `read_inter_plane_rect` (a 16x4 **chroma** unit) | re-indexed ctx **1**, symbol 0 |

**The report's "ours reads a second LUMA unit" is refuted.** That came from
`read_coeffs_rect`'s `tag=all_zero plane=0` label, which the report itself
flagged as hardcoded. A per-unit trace (`plane`, `w`, `h`, `txb_skip_ctx`,
`around`) shows the block is a **16x4 inter strip** at (64,32) and our
decoder reads exactly the unit the oracle reads — one luma 16x4 at bit 3706,
then one U 16x4 at bit 3719. The fork is a **context row**, not a unit
count. Same conclusion Zeynep-3 reached independently on H5 from the other
half of the same formula.

**Root-of-context, measured** with `EC_DCDUMP` + `EC_CHROMA_PUB`: at the
U unit our gather reported `left[9][plane 1].level = 7` (one extra luma mi
row). The oracle's `get_txb_ctx` there reads `ctx_base 0` →
`txb_skip_ctx = 0 + 7`.

## 3. The fix, measured

| | frame 0 | frame 1 |
|---|---|---|
| before | exact | 8884/36864 wrong, first Y(91,32) |
| after | exact | **byte-exact** |

2/2 frames byte-exact against the instrumented `aomdec`
(`EC_AV1_FINAL_DUMP`, decode order).

**Counter.** `decode::rect4_inter_own_chroma444_hits()` — 9 on the pinned
witness, 0 on the 4:2:0 control of the same source and flags.

## 4. Gate

`a_444_lossy_rect4_inter_stream_decodes_pixel_exact`
(`crates/ec-av1/src/stream.rs`). The first exactness gate for the 4:4:4
**LOSSY** cell — the whole committed 4:4:4 base was lossless 8-bit.

- pinned fixture, length + FNV asserted;
- chroma planes asserted full resolution (the 4:4:4 shape claim, stated
  rather than left to the byte compare to imply);
- byte-for-byte through `decode_all_frames_vs_oracle` (oracle aomdec);
- non-vacuity: the counter must be non-zero, so the corrected route ran;
- specificity: the same source and flags at 4:2:0 code 16 HORZ_4 + 28 VERT_4
  strips (asserted, so the arm cannot pass for the wrong reason), leave the
  counter at 0, and are themselves compared byte-for-byte against aomdec —
  the arm that goes red if the new arm is ever widened past `ss_x == 0`.

**Mutation proof.** Restoring the old pair arm with the counter left in
place:

```
decode-order frame 1 of 2 differs from the oracle at byte 4187
(ours 35 vs 38), 8884 bytes differ
```

— the same 8884 and the same Y(91,32) as the original red, so the gate
catches this defect and not a near-miss of it.

Second direction: widening the arm to `Some(_)` leaves the 4:2:0 control at
0 gathers, because at ss_x 1 a 1:4 strip's chroma reaches
`decode_inter_block` through a different caller and never sets
`inter_strip_chroma`. That is why the identity proof is the 4:2:0 **oracle
compare**, not the counter: the counter proves the 4:4:4 route fired, the
oracle compare proves nothing else moved.

## 5. Identity and the sweep's other divergences

Green against this `decode.rs`:

- the crate's whole `444` family — 9 tests;
- `rect_strip` family — 10 tests;
- `a_real_aomenc_inter_sequence_with_a_16_level_rect_leaf_decodes_pixel_exact`;
- all **three new chroma-format-sweep gates**, run in the sweep worktree
  `~/.cache/wt/av1fmt` with this `decode.rs` copied in and the sweep tree
  restored afterwards:
  `a_lossless_444_10bit_inter_stream_decodes_pixel_exact`,
  `a_444_12bit_inter_sequence_decodes_pixel_exact`,
  `a_real_aomenc_odd_coded_dimension_streams_decode_pixel_exact`.

Same-class recipes re-measured:

| recipe | size | before | after |
|---|---|---|---|
| **H1** 4:4:4 lossy cq20 `--limit=2` | 6441 B | frame 1: 8884 wrong | **byte-exact** |
| H1 control `--enable-rect-partitions=0` | 6529 B | exact | exact |
| **H2** 4:4:4 lossy `--sb-size=64 --min/max-partition-size=64`, 4 f | 21830 B | frames 0-1 exact, from frame 2 608 wrong, U/V only, first U(2,31) | **unchanged** — still 608 wrong, U/V only |
| **H3** 4:4:4 lossless `--tile-columns=1` 256x128, 6 f | 77847 B | sweep reported 90 chroma wrong in frame 0 | **byte-exact, 6/6** |

**H2 is not this class and is not fixed here.** Its entropy decode is
bit-identical over 140820 reads (the sweep measured it), so it is
reconstruction arithmetic, not a context: the `av1_get_adjusted_tx_size`
clamp on a 4:4:4 chroma TX_32X64 is still the prime suspect. Re-measured
here to prove the class claim above: H2's numbers are bit-identical before
and after, so this fix neither caused nor cured it.

**H3**: with the sweep's full flag set
(`--profile=1 --passes=1 --lossless=1 --end-usage=q --cq-level=20
--cpu-used=2 --threads=1 --row-mt=0 --lag-in-frames=0 --kf-max-dist=100
--limit=6 --tile-columns=1`) the 6-frame 77847-byte stream is byte-exact.
Without those flags the same source diverges hard, so the sweep's H3 recipe
was under-specified; the tile-column defect does not reproduce on the
recipe as re-derived here. Flagging, not claiming.

**H5 (lane-av1h5, Zeynep-3): out of scope and structurally disjoint.** The
new arm lives inside `match strip_chroma`, which is `Some` only from the
single setter the PARTITION_HORZ_4 / VERT_4 16x4 / 4x16 `inter_piece` loop
runs; a BLOCK_128X128 unsplit root never reaches it.

## 6. Residue handed on, not fixed here

- **`decode_rect4_16_intrabc` (`decode.rs:16466`) — same class, still
  open.** The intra-BC 16x4 / 4x16 strip reader hardcodes the 4:2:0 pair
  with no `ss` gate at all:
  `let (pw, ph) = if horz { (16, 8) } else { (8, 16) };`,
  `pair_mi = (lmi.0 - 1, lmi.1) | (lmi.0, lmi.1 - 1)`, and
  `cpx = pair_mi.1 * MI / 2` (a literal 4:2:0 halving). At ss (0,0) that
  decodes an 8x4 chroma block at a halved anchor where libaom codes a
  16x4 at the strip's own. I did not fix it: it needs a 4:4:4 witness with
  screen-content tools on (aomenc sets `allow_screen_content_tools` for
  `testsrc2` yuv444p by default), which I had no budget to produce, and
  fixing an unreproduced branch here would be a guess. The pinned H1 fixture
  decodes exactly, so it does not reach that reader.
- **H2** — 4:4:4 lossy + tx-size-search chroma ±1, entropy-clean. Own lane.
- **H4** — 4:4:4 lossless + tile rows, hard divergence. Own lane; untouched.
- **H5** — 4:4:4 10-bit lossless 128 root, chroma band/offset term. Zeynep-3.
- **H6** — 12-bit 4:4:4 needs a smoothed source. Coverage caveat, not a
  defect.

## 7. Method notes for the next lane

- `testsrc2` is time-parameterised: any sweep recipe quoting it must pin
  `rate`, or the fixture is not reproducible. `rate=1` vs `rate=25` on
  128x96 yuv444p is the difference between an 8121-byte exact stream and
  the 6441-byte divergent one.
- `read_coeffs_rect`'s `EC_COEFF_STEP tag=all_zero` prints a hardcoded
  `plane=0`. It has now mis-attributed a chroma read as a luma one in two
  independent reports. The oracle's `EC_COEFF_STEP` prints the real plane.
  Pair the two traces by the msac `post_rng` (both sides print it after the
  read) rather than by line index.
- `EC_AV1_TELL ... label=block_entry` prints the msac bit position at each
  `decode_inter_block` entry, and the oracle's `EC_SYMR` `pre` third field
  is our bit position minus a constant 15. That pair joins the two symbol
  streams to a block without any new instrumentation.
- `EC_DCDUMP` already dumps the above/left entropy contexts per plane;
  `#[track_caller]` on `Neighbours::around_mi_rect` names the gather site
  in one line if you ever need to attribute one again.
