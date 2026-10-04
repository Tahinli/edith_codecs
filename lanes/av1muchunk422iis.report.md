# lane-av1muchunk422iis — the intra-in-inter mu-chunk chroma gather at 4:2:2

Base: `main` @ `bba9767d`. Branch: `lane/av1muchunk422iis`. Local only.

**Verdict: closed.** The intra-in-inter `side > 64` mu-chunk walk
(`decode.rs:46820`) gathered its per-unit CHROMA coefficient context with the
plain luma `around_mi`, which at ss (1, 0) sums `unit_luma_w / MI` above cells
where libaom sums `txb_w_unit` — twice as many — so `dc_sign_ctx`'s signum
flipped whenever the doubled above vote cancelled the left one. Routed through
`around_mi_422_chroma` with the unit's PER-AXIS luma footprint
(`unit_luma_w`, `unit_luma_h`) exactly as the compound twin already was, the
hunted stream `i5_grad256_iis.obu` is now **byte-exact against ffmpeg over all
6 shown frames, every plane**, and byte-exact against the decode-order oracle
over all 7 frames (6 shown + 1 hidden alt-ref). Restoring the square gather reds
the new gate on the exact sample the class report named.

## 1. The change

One site, `crates/ec-av1/src/decode.rs` in the intra-in-inter `side > 64`
mu-chunk unit loop:

```rust
let cu_around = if ss_x(fctx) == 1 && ss_y(fctx) == 0 {
    neighbours.around_mi_422_chroma(unit_mi, unit_luma_w, unit_luma_h)
} else {
    neighbours.around_mi(unit_mi, unit_luma_w)
};
```

The condition is the same ss (1, 0) gate the eight sibling chroma gathers use
(`decode.rs:15441, 18268, 23924, 24282, 24956, 43980, 45810`). At ss (1, 0)
`unit_luma_w = cu_tx << 1 = 64` and `unit_luma_h = cu_tx << 0 = 32`, so
`around_mi_422_chroma` samples every second above cell (libaom's
`txb_w_unit`-cell sum) and takes the full `unit_luma_h` left extent
(unsubsampled at ss_y 0). At 4:2:0 (ss 1,1) and 4:4:4 (ss 0,0) the two luma
extents are equal, so the `else` branch is the previous call verbatim — the
440/444 arms are unchanged text on an untaken path.

Not touched: the compound walk, c3, c4.

## 2. The pinned stream

`crates/ec-av1/fixtures/422_muchunk_intra_in_inter_128x256.obu`

| | |
|---|---|
| sha256 | `64794f278eee738dcc59d90dc0c7c4ef2828c5b7c13724760a33be11f224908e` |
| bytes | 667 |
| fnv1a64 | `0x2bee_eba3_052b_f9a3` |
| source | `~/.cache/av1muchunk422b/i5_grad256_iis.obu` (the hunt's file; NOT re-encoded) |
| shape | `(use_128x128_superblock, ss_x, ss_y, mono) = (true, 1, 0, false)` |
| frames | 6 shown + 1 hidden alt-ref (decode order 7) |

Recipe (same aomenc argv as the 128x128 twin `422_muchunk_intra_in_inter_128root.obu`,
different source):

```text
ffmpeg -v error -f lavfi -i "gradients=size=128x256:c0=0xa1128f:c1=0xd98c48:\
  c2=0x120601:c3=0x4abfba:seed=63:duration=0.24:rate=25,\
  noise=all_seed=63:alls=6:allf=t" -t 0.24 -pix_fmt yuv422p \
  -f yuv4mpegpipe - > grad256.y4m
aomenc --codec=av1 --profile=2 --input-chroma-subsampling-x=1 \
  --input-chroma-subsampling-y=0 --passes=1 --end-usage=q --cq-level=62 \
  --cpu-used=0 --threads=1 --row-mt=0 --sb-size=128 \
  --min-partition-size=128 --max-partition-size=128 \
  --enable-rect-partitions=0 --enable-ab-partitions=0 \
  --enable-1to4-partitions=0 --enable-palette=0 --enable-intrabc=0 \
  --deltaq-mode=0 --enable-tx-size-search=0 --enable-interintra-comp=1 \
  --enable-interintra-wedge=1 --enable-smooth-interintra=1 --obu -o X
```

## 3. Measurement — per-frame, per-plane vs ffmpeg

`ffmpeg -v error -i X.obu -pix_fmt yuv422p -f rawvideo` vs
`dump_yuv X.obu <out>`, differing samples per shown frame (Y / U / V):

| stream | f0 | f1 | f2 | f3 | f4 | f5 |
|---|---|---|---|---|---|---|
| **pre-fix** (square gather) | 0 | 0 | V **2105** | 0 | V 1982 | Y 24155 U 12162 V 12091 |
| **post-fix** | 0 | 0 | 0 | 0 | 0 | 0 |

Pre-fix first wrong sample: V (row 190, col 36) of shown frame 2 (V index
`190 * 64 + 36 = 12196`), **ours 147 vs ffmpeg 146**. Post-fix that sample reads
146 and shown frame 2's whole V plane hashes
`fnv1a64 = 0xa7ba_8711_9d13_8fe5`, equal to ffmpeg's own bytes.

(The class report's prose wrote the pair as "ours 146 vs ffmpeg 147"; the
measured pair on this host is ours-147/ffmpeg-146 pre-fix. Same location, same
one-sample disagreement, mirrored wording.)

## 4. The gate

`crates/ec-av1/src/stream.rs`
`a_422_muchunk_intra_in_inter_128x256_stream_is_exact_and_reaches_its_unit_walk`:

1. **Counter witness** — `mu_chunk_intra_in_inter_units()` delta `96 >= 96`
   (the walk ran), `mu_chunk_compound_units()` delta `0` (this stream never
   enters the compound arm).
2. **Oracle-free pin** — shown frame 2 V plane length `64 * 256`, sample
   `V[12196] == 146`, whole-V-plane fnv `0xa7ba_8711_9d13_8fe5`. Cannot
   silently skip.
3. **Required ffmpeg compare** — every plane of every shown frame equal to
   `ffmpeg_decode_sequence_422` (hard-fails under `EC_AV1_REQUIRE_FFMPEG`;
   prints a loud SKIP-note otherwise).
4. **Decode-order oracle compare** — `decode_all_frames_vs_oracle` when aomdec
   is present (7 frames).

### Red-before-green

Restoring the call to `neighbours.around_mi(unit_mi, unit_luma_w)` reds exactly
the pin:

```
panicked at stream.rs:43404:
  ... shown frame 2 V[12196] (chroma (190, 36)) is 147 -- the pre-fix square gather read 147 here
  left: 147  right: 146
```

Green after the fix (all three mu-chunk gates + the 4:4:4 twin):

```
a_422_muchunk_compound_128root_pinned_stream_reaches_its_unit_walk ... ok  (compound_units=32, intra=0)
a_422_muchunk_intra_in_inter_128root_pinned_stream_is_exact_and_reaches_its_unit_walk ... ok
a_422_muchunk_intra_in_inter_128x256_stream_is_exact_and_reaches_its_unit_walk ... ok  (intra_in_inter_units=96)
a_444_intra_in_inter_128root_muchroma_pinned_stream_decodes_pixel_exact ... ok
```

## 5. Repro

```
git worktree add -b lane/av1muchunk422iis ~/.cache/wt/av1muchunk422iis origin/main   # bba9767d
cd ~/.cache/wt/av1muchunk422iis
export CARGO_TARGET_DIR=$HOME/.cache/tgt-av1muchunk422iis TMPDIR=$HOME/.cache/tmp
export EC_AV1_REQUIRE_FFMPEG=1 EC_AV1_AOMDEC=$HOME/.cache/aom-oracle/build/aomdec
cargo test -p ec-av1 --lib muchunk -- --nocapture
```

Raw ffmpeg compare (outputs under `$HOME`, never `/tmp` — this host's `/tmp` is
over quota):

```
S=crates/ec-av1/fixtures/422_muchunk_intra_in_inter_128x256.obu
ffmpeg -v error -y -i $S -pix_fmt yuv422p -f rawvideo $HOME/.cache/tmp/i5.ref.yuv
$CARGO_TARGET_DIR/debug/examples/dump_yuv $S $HOME/.cache/tmp/i5.ours.yuv
# per-frame 65536-byte planes: Y 32768, U 16384, V 16384
```

## 6. Not claimed

- The class is not declared closed beyond this walk: c3/c4 remain diverging and
  were not touched. This lane fixes and pins the intra-in-inter `side > 64`
  mu-chunk gather only.
- Nothing about 4:2:0 or 4:4:4 was changed; their identity is structural (the
  `else` branch is the old call on an untaken path) and spot-checked on the
  4:4:4 twin above.
