# lane-av1-ibc128chunk — the 128-root intrabc strip's square-cut mu-chunk chroma walk, de-square-cut and WITNESSED

Branch `lane-av1-ibc128chunk`, parent `a7d22aec`. Target dir
`$HOME/.cache/cargo-target-av1ibc128chunk` throughout. No push.

## Verdict

The deferred copy is fixed **and witnessed** — this is not a defensive fix.
`decode_intrabc_128rect` cut each mu chunk's chroma plane block with
`chunk_chroma = cside * 64 / side` (two sites, decode.rs:12663 and decode.rs:12896
at the parent), the same square-cut defect lane-av1-422bigblock fixed and
witnessed in `decode_inter_block` and `decode_block_128rect`. Red before: a real
4:2:2 `aomenc --enable-intrabc` sb128 stream loses half of every mu chunk's
chroma units and its frames 0 and 3 diverge from `aomdec`, first difference at
the top-left corner of the very first 128x64 intrabc block. Green after: the
same stream decodes all five frames byte-identical to `aomdec --rawvideo`, and
the red build's 4 lost blocks are recovered.

Both pinned witnesses are committed; committed code still refuses 4:2:2 at the
sequence header and the `EC_AV1_ALLOW_422_PROBE` bypass was applied and
reverted per probe run (`git diff` on the refusal line is empty; the only
`EC_AV1_ALLOW_422_PROBE` strings left in the tree are two doc-comment mentions
of the recipe).

## The defect (both sites)

`decode_intrabc_128rect` is the intrabc arm of `decode_block_128rect`: a
128x64 / 64x128 / 128x128 strip coded with `use_intrabc`, predicted from this
frame's own reconstruction, reconstructed through the INTER residual walk
(`decode_token_recon_block`'s else arm).

A 64x64 mu chunk's chroma plane block is **per-axis** `(64 >> ss_x) x
(64 >> ss_y)`: 32x32 at 4:2:0 (one TX_32X32 per plane per chunk), **32x64 at
4:2:2 (TWO STACKED TX_32X32 per plane per chunk)**, 64x64 at 4:4:4 (four).
`av1_get_max_uv_txsize` caps the unit at 32 on each axis. The square-cut model
walked a `chroma_side`-square chunk, i.e. the 4:2:0 count on both axes.

Measured consequences at 4:2:2 (witness below):

1. **Half the units.** One chroma unit per plane per chunk instead of two
   stacked — the chunk's lower 32 chroma rows got no residual at all.
2. **Wrong extent, twice.** The block's own chroma plane block was still
   `bw / 2 x bh / 2` (`cpx, cpy = px / 2, py / 2`), i.e. 4:2:0-shaped, even
   though the same function's two `mv_to_q4(cpy, dv_row, ss_y(fctx))` calls
   already scaled the DV on the y axis by **its own** subsampling — the
   prediction window was half as tall as the motion vector asked for.
3. **Wrong entropy spans.** `around_mi` / `record_mi_chroma` stamped every unit
   over a `unit_luma = cu_tx * (side / cside)` = 16x16-mi luma square instead
   of the per-axis 16x8 at 4:2:2, and the `mu_chroma` per-unit replay walked a
   32-row step down a 64-row chunk.
4. **Entropy lock lost.** The halved unit count desynced the tile: the red build
   decoded 2 of the stream's 6 intrabc blocks; the fixed build decodes all 6.

## The fix (mirror of the two witnessed twins)

Both sites, per-axis, in the same expression shape
`decode_inter_block` already carries:

| old | new |
|---|---|
| `let (cpx, cpy) = (px / 2, py / 2);` | `(px >> ss_x(fctx), py >> ss_y(fctx))` |
| `let (cw, ch) = (bw / 2, bh / 2);` | `(bw >> ss_x(fctx), bh >> ss_y(fctx))` |
| `let chunk_chroma = cside * 64 / side;` | `chunk_chroma_w = (64usize) >> ss_x(fctx)` / `chunk_chroma_h = (64usize) >> ss_y(fctx)` |
| `let unit_luma = cu_tx * (side / cside);` | `unit_luma_w = cu_tx << ss_x(fctx)` / `unit_luma_h = cu_tx << ss_y(fctx)` |
| `units_x/y = chunk_*_mi / (unit_luma / MI)` | same, on `unit_luma_w` / `unit_luma_h` |
| `around_mi(unit_mi, unit_luma)` | `around_mi_rect(unit_mi, unit_luma_w, unit_luma_h)` |
| `record_mi_chroma(unit_mi, unit_luma, unit_luma, ..)` | `.. unit_luma_w, unit_luma_h, ..` |
| `cu_x = cpx + cc * chunk_chroma + uc * cu_tx` | `.. cc * chunk_chroma_w + ..` |
| `cu_y = cpy + cr * chunk_chroma + ur * cu_tx` | `.. cr * chunk_chroma_h + ..` |
| `cu_pred` / composed-grid `dst` offsets | per-axis offsets at the square `cside` stride |
| `read_inter_chroma_lossless(.., (cc*cc_, cr*cc_), (cc_, cc_), ..)` | per-axis org and region |
| replay `unit_luma = cu * 2` | `unit_luma_w = cu << ss_x(fctx)` / `unit_luma_h = cu << ss_y(fctx)` |
| replay `ch / chunk_chroma`, `cw / chunk_chroma` | `ch / chunk_chroma_h`, `cw / chunk_chroma_w` |
| `EC_IBC128_BANDS` trace spans | per-axis |

New counters (both exported from `stream` and printed by `decode_probe`):
`INTRABC_128RECT_CHROMA_CHUNK_HITS` (one bump per chroma `read_inter_plane` of
this walk) and `INTRABC_128RECT_CHUNK_SHAPE`
(`(64 >> ss_x) << 8 | (64 >> ss_y)` of the last chunk walked).

## Fixtures (pinned, `git add -f`; `fixtures/` is gitignored)

| fixture | bytes | sha256 | fnv1a64 |
|---|---|---|---|
| `422_intrabc_sb128_strip.obu` | 1672 | `f92000db86df7acc577d60dd1d0d97454bff4705799ea59d9de34eb5a71dbbcd` | `0x50f5cfc576e4cd00` |
| `422_intrabc_sb128_strip_notxsearch.obu` | 1675 | `80dd0d4e93fd6c3d4b7188a43e69d9638f2bece38edfab60ff3582628314b5ec` | `0xd4936f252ff8cff0` |

Source (the deterministic `geq` sinusoid the tree's own sb128 intrabc-rect gate
uses, re-cut at `yuv422p` — the 4:2:2 twin of that gate's arm 1):

    ffmpeg -v error -f lavfi -i "nullsrc=size=384x320:rate=25,format=yuv420p,\
      geq=lum='128+90*sin((X+2*N)/37)+30*sin(Y/29)':\
      cb='128+30*sin(X/23)':cr='128+30*sin((Y+N)/19)'" \
      -pix_fmt yuv422p -strict -1 -t 0.2 -f yuv4mpegpipe -y geq/384x320.y4m

    ~/.cache/aom-oracle/build/aomenc --codec=av1 --i422 --profile=2 --passes=1 \
      --end-usage=q --cq-level=32 --cpu-used=0 --threads=1 --row-mt=0 \
      --lag-in-frames=0 --kf-max-dist=1 --limit=5 --tile-columns=0 --sb-size=128 \
      --min-partition-size=64 --enable-rect-partitions=1 --enable-ab-partitions=0 \
      --enable-1to4-partitions=0 --enable-palette=0 --tune-content=screen \
      --enable-intrabc=1 --deltaq-mode=0 --enable-tx-size-search=1 \
      --obu -o geq/384x320_cq32_ts1.obu geq/384x320.y4m

`_notxsearch` is the same command with `--enable-tx-size-search=0`. Both are
pinned and both are red-before / green-after (the tx-size-search arm is not
required — a mis-signed flag on a witness is a coin flip, so both went in).

## Red before / green after (measured)

`aomdec` is `~/.cache/aom-oracle/build/aomdec`; ours is `decode_probe` (debug,
`--features gate-counters`) on the `EC_AV1_ALLOW_422_PROBE` patch-run-restore
build, compared with `cmp` on the full raw output.

| fixture | build | 128-root blocks | chroma units stamped | vs `aomdec` |
|---|---|---|---|---|
| `422_intrabc_sb128_strip.obu` | parent `a7d22aec` | **2** | **8** | **DIFF** |
| `422_intrabc_sb128_strip.obu` | this lane | **6** | **48** | **EXACT** (5/5 frames) |
| `422_intrabc_sb128_strip_notxsearch.obu` | parent `a7d22aec` | **2** | **8** | **DIFF** |
| `422_intrabc_sb128_strip_notxsearch.obu` | this lane | **6** | **48** | **EXACT** |

Frame-level detail, per fixture (245760 samples/frame: 384x320 Y + 2 x 192x320
chroma), re-measured per fixture after review — the first cut of this section
reported the `_notxsearch` arm's counts under the primary witness:

| fixture | build | f0 | f1 | f2 | f3 | f4 |
|---|---|---|---|---|---|---|
| `422_intrabc_sb128_strip.obu` (primary) | parent `a7d22aec` | **46891** | 0 | 0 | **44842** | 0 |
| `422_intrabc_sb128_strip.obu` (primary) | this lane | 0 | 0 | 0 | 0 | 0 |
| `422_intrabc_sb128_strip_notxsearch.obu` | parent `a7d22aec` | **45135** | 0 | 0 | **44540** | 0 |
| `422_intrabc_sb128_strip_notxsearch.obu` | this lane | 0 | 0 | 0 | 0 | 0 |

Both fixtures' red first difference is byte 98368 = frame 0, Y **row 256,
col 64** — the top-left of the first 128x64 intrabc block (traced at
`mi_row=64 mi_col=0`, `py = 64*4 = 256`). Frames 1, 2, 4 carry no intrabc
block and are exact in every build. Green: `cmp` identical on all
245760 x 5 bytes, 5 frames decoded, 0 hidden.

The red build reached only 2 of the 6 blocks: reading 4 chroma units where
libaom codes 8 desynced the tile, and the remaining 4 blocks were misparsed.
Recovering them is the green signal, not just the pixel count.

`EC_ECDUMP` per-unit stamps on the primary witness, red vs fixed, same entropy
position (`rng=49296` at the first block in both):

    red    4 stamps  (1 per plane per chunk, 2 chunks)
    fixed  8 stamps  (2 per plane per chunk, 2 chunks)

## Rework after review (r2) — the `mu_chroma` replay's chunk/unit step

Review r1 FAILed the first cut on a real coverage regression I introduced, and
corrected this report's red counts (done above; the 45135/44540 pair was the
`_notxsearch` arm, the primary's are 46891/44842).

**The defect.** The first cut gave the replay per-axis *extents* but kept the
old *step*: `rows`/`cols` count mu CHUNKS (`ch / chunk_chroma_h`), while the
loop body advanced the mi origin and read the grid PER UNIT
(`cr * (unit_luma_h / MI)`, grid `(cr * chunk_chroma_h)`). Those agree only when
a chunk holds exactly one unit. At 4:2:0 (`chunk_chroma == cu == 32`) it is
line-identical to the parent, so nothing measured it; at 4:2:2
(`chunk_chroma_h = 64 = 2 * cu`) the body reached only the FIRST of the two
stacked units per chunk, so rows `mi_r+8 .. mi_r+16` kept the composed-grid
smearing the replay exists to undo. The parent reached those rows (with a wrong
span but full coverage), so this was a coverage regression *my* patch added,
not a pre-existing gap.

**The reshape**, in the twin's chunk-then-unit loop
(`decode_inter_block`'s `mu_chroma_units`, decode.rs:37456-37494,
lane-av1-444 c8):

    let (ur, uc) = (chunk_chroma_h / cu, chunk_chroma_w / cu);
    for cr in 0..rows { for cc in 0..cols {
      for kr in 0..ur { for kc in 0..uc {
        unit_mi = (mi_r + (cr * 64 + kr * unit_luma_h) / MI,
                   mi_c + (cc * 64 + kc * unit_luma_w) / MI);
        (cy, cx) = (cr * chunk_chroma_h + kr * cu, cc * chunk_chroma_w + kc * cu);
        ... let start = (cy + rr) * cside + cx;

`cr * 64` is the mu chunk's LUMA-px step, which is 64 on both axes at every
subsampling — that is why the twin can write it unshifted. At 4:2:0 `ur`/`uc`
are 1, so `kr`/`kc` are 0, `unit_mi` collapses to `mi_r + cr*16`, and the grid
origin to `(cr*32, cc*32)`: **line-identical to the pre-rework walk**. The new
gate pins the shape, and the mutation proof below shows it is load-bearing.

**The LOSSLESS arm is pre-existing and deliberately untouched.** On a lossless
frame `cu` is 4 and the composed grid is a raster of TX_4x4 units, so the
chunk-counted `rows`/`cols` do not enumerate it at all: that arm re-stamps only
the 4x4 at each chunk's top-left with a 2x2-mi span. The twin raster-walks the
whole plane block instead (lane-av1-llinter). It is left as-is on purpose: it is
a **4:2:0** defect, not a subsampling square-cut, so it is out of this lane's
class, and fixing it would change 4:2:0 LOSSLESS output — which this lane's
byte-identity requirement forbids doing on a reconstruction I have no witness
to verify. Measured unreachability: a temporary env-gated counter in the arm
records **0 hits across the whole `lossless` and `intrabc` gate battery**
(13 + 19 tests). Unblock: one lossless sb128 4:2:0 stream coding a NON-SKIP
128-root intrabc strip (`--lossless=1 --sb-size=128 --min-partition-size=64
then the twin's raster reshape plus a witness. Carried as a named pre-existing,
not as a silent skip.

**Re-measured after the reshape (r2):** both witnesses byte-identical to
`a7d22aec`-relative tip r1's output (6 blocks, 48 chroma units, 5/5 frames exact
vs `aomdec`) — the reshape is a pure coverage repair, no output change on any
witness. `an_sb128_rect_strip_with_intrabc_decodes_pixel_exact`,
`an_sb128_screen_stream_with_intrabc_decodes_pixel_exact`,
`a_lossless_sb128_rect_intra_block_decodes_sample_exact`,
`a_444_sb128_root_rect_stream_with_restoration_decodes_pixel_exact`,
`the_pinned_422_bigblock_witnesses_are_present_and_refuse_by_name` and both new
gates: **7 passed / 0 failed**. Two mutation proofs on the source-scan arm,
both FAILING it: forcing `let (ur, uc) = (1, 1);` (the per-chunk-only shape)
gives `found 0`; re-introducing `cside * 64 / side` at the read-walk site gives
`left: 1, right: 0`. Restored, the gate passes.

## Reproduce (patch-run-restore, local only)

    python3 - <<'EOF'
    p='crates/ec-av1/src/stream.rs'; s=open(p).read()
    s=s.replace("    if seq.subsampling_x != seq.subsampling_y {",
                "    if seq.subsampling_x != seq.subsampling_y && !crate::envflags::env_flag!(\"EC_AV1_ALLOW_422_PROBE\") {",1)
    open(p,'w').write(s)
    EOF
    CARGO_TARGET_DIR=$HOME/.cache/cargo-target-av1ibc128chunk \
      cargo build -p ec-av1 --example decode_probe --features gate-counters
    EC_AV1_ALLOW_422_PROBE=1 EC_ECDUMP=1 \
      $HOME/.cache/cargo-target-av1ibc128chunk/debug/examples/decode_probe \
      crates/ec-av1/fixtures/422_intrabc_sb128_strip.obu 2>&1 | grep -c EC_IBC128_STAMP
      -> 48   (parent: 8)
    EC_AV1_ALLOW_422_PROBE=1 EC_PROBE_OUT=/tmp/ours.raw \
      $HOME/.cache/cargo-target-av1ibc128chunk/debug/examples/decode_probe \
      crates/ec-av1/fixtures/422_intrabc_sb128_strip.obu
    ~/.cache/aom-oracle/build/aomdec --rawvideo -o /tmp/oracle.raw \
      crates/ec-av1/fixtures/422_intrabc_sb128_strip.obu
    cmp /tmp/ours.raw /tmp/oracle.raw   # identical
    git checkout -- crates/ec-av1/src/stream.rs

## Gates

- `the_pinned_422_intrabc_sb128_strip_witnesses_refuse_by_name` (stream.rs) —
  both fixtures' byte pins (`len` + `fnv1a64`) and the unconditional
  refusal-by-name contract. The decode-level assertions above cannot run in
  committed code (the 4:2:2 refusal stands), so they are probe-measured and
  reproducible from the recipe; this is the same split
  `the_pinned_422_bigblock_witnesses_are_present_and_refuse_by_name` uses.
- `the_intrabc_128rect_chroma_chunk_walk_stays_per_axis` (stream.rs) — the
  source-scan arm. Scans `decode_intrabc_128rect`'s body **with comments
  stripped** (the fix's own comments quote the old expression to say what it
  was) and pins: zero `cside * 64 / side`; the `chunk_chroma_w/h` pair at
  **both** de-square-cut sites (read walk and `mu_chroma` replay); the replay's
  chunk-then-unit shape (`let (ur, uc) = (chunk_chroma_h / cu, chunk_chroma_w /
  cu);`, `for kr in 0..ur {`, `for kc in 0..uc {` — the r2 coverage fix); the
  `unit_luma_w/h`, `cw/ch`, `cpx/cpy` and `around_mi_rect` forms once each; the
  replay's own `cu << ss` pair; the per-axis unit arithmetic
  (`(64>>ss_x)/32 * (64>>ss_y)/32` = 1 / 2 / 4 at 4:2:0 / 4:2:2 / 4:4:4); and
  both new counter accessors. **Non-vacuity measured twice**: forcing
  `let (ur, uc) = (1, 1);` (the per-chunk-only shape) fails it with
  `found 0`, and re-introducing `let chunk_chroma_w = cside * 64 / side;` at
  the read-walk site fails it with `left: 1, right: 0`; restored, it passes.

## Byte-identity outside 4:2:2

- `an_sb128_rect_strip_with_intrabc_decodes_pixel_exact` — **green**, and it
  is the right gate: same deterministic `geq` recipe as the witness, at
  `yuv420p` / `yuv420p10le`, all three arms decode-order pixel-exact vs the
  oracle with `intrabc_128rect_hits=+14 / +1 / +15` (30 blocks through the
  changed walk, `skip=false`, real residual reads). At ss (1,1) every new
  expression reduces to the old constant: `64 >> 1 = 32`, `32 << 1 = 64`,
  `bw >> 1 = bw / 2`, 16x16-mi spans.
- `an_sb128_screen_stream_with_intrabc_decodes_pixel_exact` — green (8 arms,
  4-6 intrabc blocks each).
- `a_444_sb128_root_rect_stream_with_restoration_decodes_pixel_exact` — green.
- `a_lossless_sb128_rect_intra_block_decodes_sample_exact` — green.
- `a_real_aomenc_intrabc_mixed_vartx_tree_decodes_without_the_mixed_leaf_refusal`
  and `a_real_aomenc_screen_key_frame_reads_use_intrabc_on_rect_strips` — green.
- `the_pinned_422_bigblock_witnesses_are_present_and_refuse_by_name` — green
  (the twin lane's fixtures are untouched).
- Scoped battery: `intrabc` filter 19 passed / 0 failed / 1 ignored;
  `422` + `444` filters 12 passed / 0 failed. `cargo check -p ec-av1
  --all-targets`: 0 warnings.

**DISCLOSED 4:4:4 behavior change.** At ss (0,0) the walk now codes FOUR
TX_32X32 units per mu chunk per plane (1x1 -> 2x2) where the parent coded one,
and the block's chroma plane block is now luma-sized (`bw x bh`) instead of
`bw/2 x bh/2`. This is the same disclosure lane-av1-422bigblock made for
`decode_block_128rect`: the parent's walk was **provably wrong** at ss (0,0) —
`cside = (side/2).max(...)` is the 4:2:0 half-side, so the old 32x32 chunk
covered a quarter of a 64x64 4:4:4 chroma chunk and the origin `px/2` was off by
half a superblock. Unlike that lane, this one also changes the *block* shape at
4:4:4 (`cw/ch`, `cpx/cpy`), so the ticket's "spans are equal at (0,0)"
premise does not hold for this function, whose extents were 4:2:0-hardcoded
throughout. Measured unreachability: **no committed 4:4:4 fixture engages
`decode_intrabc_128rect` at all** (`ibc128=0` on all six 4:4:4 fixtures
including `444_sb128rect_lr_witness.obu` and `ll444_ibc_rect.obu`), so no
green gate covers or contradicts the change; it is landed on the same geometry
argument that the two reviewed twins landed on, and the 4:4:4 route needs its
own witness (`aomenc --i444 --profile=1 --sb-size=128 --enable-rect-partitions=1
--enable-intrabc=1` over content that makes aomenc take a 128-root PARTITION_NONE
or HORZ/VERT strip — no committed 4:4:4 fixture does).

## Hunt recipe sweep (what was tried before the hit)

`--profile=2 --sb-size=128 --enable-intrabc=1` at 4:2:2, all with
`--i422`, `--tune-content=screen`, `--enable-palette=0`, `--passes=1`,
`--lag-in-frames=0`, `--kf-max-dist=1`, `--min-partition-size=8`,
`--max-partition-size=128`, `--enable-rect-partitions=1`,
`--enable-1to4-partitions=1`:

- **lavfi `testsrc2 / smptebars / mandelbrot / life / rgbtestsrc /
  smptehdbars`, 128x128 and 256x256/384x256/512x256/128x128, `cpu-used` 0/2/3/4,
  `cq-level` 15-60, 2x2/3x2/4x4 `tile=`** (24 encodes + 24 x 2 x 8 = ~400
  encodes): 9 streams reached `decode_intrabc_128rect`; **all 9 reached it with
  a frame that was ALREADY desynced** before the block. The blocker is
  pre-existing and unrelated: at 4:2:2 a chroma tx unit goes non-square
  (TX_16X8 etc.) and the merged tree does not read the `tx_type` the oracle
  codes for it — first divergence on `c_testsrc2_cq40.obu` at coefficient unit
  **4** (plane 1, `mi_row=0 mi_col=0`, `tx_size=5` = TX_16X8: the oracle emits
  `tx_type` after `all_zero` where ours goes straight to `eob`, rng 55680 /
  34568 identical up to that point). **Named for its owning lane, not chased
  here.**
- **the tree's own gate recipe re-cut at `yuv422p`** (the `geq` sinusoid
  sources, 384x320 cq32 and 512x512 cq45, `enable-tx-size-search` 0/1): the
  smooth-chroma `geq` content avoids the non-square-chroma-tx defect, and both
  cq32 arms are the red/green witnesses above. cq20/45/55 code no 128-root
  intrabc block at all (`ibc128=0`), so the hit is a cq32 window, not a
  content accident.

Control that clean 4:2:2 witnesses do exist on this tree (so the "already
desynced" arms above were a content choice, not a tree-wide wall):
`422_sb128_3f.obu` decodes byte-exact vs `aomdec` here.
