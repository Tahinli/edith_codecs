# lane-av1muchunkluma — c3 and c4: the first wrong block, named and gated

Base: `main` @ `c2e592e3`. Branch: `lane/av1muchunkluma`. No decode code is
changed by this lane: the two streams were already closed by a landed fix, so
the deliverable is the **pin + gate** that says so, plus the red-before proof.

**Verdict: both c3 and c4 are byte-exact on current `main`, and the site that
owns both is the intra-in-inter `side > 64` mu-chunk chroma gather
(`crates/ec-av1/src/decode.rs:46834`, the `around_mi_422_chroma` call), fixed by
`bd762046` (lane-av1muchunk422iis).** Neither stream needed an edit — the
class report's tree predates that fix. Both are now pinned as fixtures and
gated; surgically restoring the plain `around_mi` gather reds BOTH gates on the
luma plane of shown frame 1, the frame the class report named.

## 1. The bytes

The hunt's bytes still exist at `~/.cache/av1muchunk422b/`, byte-equal to the
shas in `lanes/av1muchunk422class.report.md` (zero aomenc attempts were needed
to find them):

| stream | sha256 | bytes |
|---|---|---|
| `c3_grad128_comp.obu` | `e801ce5596c9ed72e7af9c5428b39aa3730262b9a24220f1e05b0f1992acd9c7` | 34449 |
| `c4_grad256_comp.obu` | `90232481d39afa5389501feaee11645390fc714635249381f077749e616a0fd5` | 68925 |

Both are 4:2:2 (ss 1/0), `part128 split=0`, 128 root, 6 shown frames.

## 2. The first wrong sample is still luma — and it is gone

`ffmpeg -v error -i X.obu -pix_fmt yuv422p -f rawvideo` (the pixel oracle) vs
`dump_yuv X.obu <out>`, compared per plane per shown frame:

| stream | tree | first wrong sample | plane diff counts (f0..f5) |
|---|---|---|---|
| `c3_grad128_comp` | pre-fix (`dc2deff2` binary) | **Y (62, 6) f1**, ours 74 / ffmpeg 75 | 0, Y8145 U5088 V6079, Y8168 U5107 V6131, Y16190 U8090 V8120, Y16220 U8117 V8113, 0 |
| `c3_grad128_comp` | `main` `c2e592e3` | **none** | all planes, all 6 frames: EXACT |
| `c4_grad256_comp` | pre-fix | **Y (126, 3) f1**, ours 94 / ffmpeg 93 | 0, Y16280 U8143 V8138, Y32747 …, Y32480 …, Y32448 …, Y24415 … |
| `c4_grad256_comp` | `main` | **none** | all planes, all 6 frames: EXACT |

The pre-fix column reproduces the class report's numbers exactly (`c3` f1 Y
first at (62, 6); `c4` first at (126, 3)), so the control is the class report's
own tree: the `dc2deff2` binary is the sym-DC-sign-fixed, iis-unfixed build
`~/.cache/tgt-av1muchunk422class/debug/examples/dump_yuv`. Luma-first confirmed
on both, at the exact coordinates the report gave.

## 3. The owning block

Both streams' only mu-chunk walk that matters is site 8, the **intra-in-inter
`side > 64`** per-unit chroma walk:

Counts here are the committed counter's per-TX_32X32-UNIT deltas, not the
hunt's per-mu-chunk probe lines (the hunt's 16 / 24 lines are 4 units each on
this walk, e.g. the landed 128x256 iis fixture: 24 probe lines, 96 units).

- `c3`: 64 counter units, 0 compound-named units. Its content was encoded with
  the compound arm's flags (`--enable-masked-comp=1 --enable-onesided-comp=1
  --auto-alt-ref=1`) but its partition search never enters the compound mu-chunk
  walk (0 `compound/singleref` probe lines), so site 8 is its only one.
- `c4`: 96 intra-in-inter units, 0 compound-named units; it additionally takes
  the **single-reference twin** (the class report's site 5, 24 probe lines with
  `compound=false`), which the `MU_CHUNK_COMPOUND_UNITS` counter by design does
  not name.

The gather that owned it is a single site, `decode.rs:46834` inside
`decode_inter_block`'s intra-in-inter `side > 64` unit loop: it read
`neighbours.around_mi(unit_mi, unit_luma_w)`, which at ss (1, 0) sums
`unit_luma_w / MI` above luma cells where libaom sums `txb_w_unit` — twice as
many — flipping `dc_sign_ctx`'s signum. `bd762046` routed it through
`around_mi_422_chroma(unit_mi, unit_luma_w, unit_luma_h)` under the ss (1, 0)
gate; 4:2:0 / 4:4:4 keep the plain gather. **This lane did not edit it.**

## 4. Pinned and gated

Two new fixtures (`crates/ec-av1/fixtures/`) and two new tests
(`crates/ec-av1/src/stream.rs`), following the lane-av1muchunk422iis pattern:

| fixture | len | sha256 |
|---|---|---|
| `422_muchunk_iis_128root_grad128.obu` | 34449 | `e801ce5596c9ed72…` |
| `422_muchunk_iis_128x256_grad256.obu` | 68925 | `90232481d39afa53…` |

Provenance: both were **re-encoded by this lane** from the frozen sources with
the hunt's own argv (`run.sh` `BASE` + `--cq-level=40` + `COMP`) and are
byte-identical to the hunt's `.obu`; the sources also regenerate
byte-for-byte (lavfi `gradients` + `noise`, seed 63):

- `grad128.y4m` sha256 `ab8b23137c4efc818884efcf0bb6ccda363bae4b8360f2ca10ca68506cbb777d`
- `grad256.y4m` sha256 `03c7fab7a6abc4e6764836cd2d35e76489438ecdba4ca1167fcd868250c0464c`

Each gate asserts, in order: pinned length + FNV; 4:2:2 (ss 1/0) sequence-header
shape; the shown-frame count; the intra-in-inter counter delta (c3 64, c4 96)
and that the compound-named counter did not move; the **oracle-free** half —
shown frame 1's luma plane FNV plus the single sample that moved first (75 /
93), with shown frame 0 as the unaffected control — and then the required
ffmpeg compare of every plane of every shown frame, plus the decode-order aom
oracle compare (7 frames, 1 hidden alt-ref).

Green at `main`:

```
cargo test -p ec-av1 --lib muchunk        # 5 passed (this lane's 2 + the 3 landed lanes')
```

## 5. Red-before (the fix reverted)

Surgical revert of `bd762046`'s one hunk (the `cu_around` expression back to the
plain `neighbours.around_mi(unit_mi, unit_luma_w)`) reds both gates, on the
luma plane of shown frame 1 — the frame the class report named:

```
a_422_muchunk_iis_128root_grad128_comp_stream_…: shown frame 1's luma moved —
  the intra-in-inter mu-chunk walk's 4:2:2 chroma context gather is wrong again
  (first sample to move is Y (62, 6), index 7942)
a_422_muchunk_iis_128x256_grad256_comp_stream_…: … (first sample to move is
  Y (126, 3), index 16131)
```

`decode.rs` was restored from a byte copy afterwards and `git diff` shows no
change to it; the two gates are green again (run above).

## 6. Acceptance

- **c3**: gated (`422_muchunk_iis_128root_grad128.obu`) and exact against
  ffmpeg on all 6 shown frames and against the decode-order oracle on 7.
- **c4**: the same site owns it (its first wrong sample is closed by the same
  fix), so it is added to the gate as its own test
  (`422_muchunk_iis_128x256_grad256.obu`), exact on the same two comparisons.
- **No edit to the decoder**: the owning site was already fixed on `main`; the
  only changes are the two fixtures, the two tests and this report.

## 7. Repro

```
git worktree add -b lane/av1muchunkluma ~/.cache/wt/av1muchunkluma c2e592e3
cd ~/.cache/wt/av1muchunkluma
CARGO_TARGET_DIR=$HOME/.cache/tgt-av1muchunkluma cargo build -p ec-av1 --example dump_yuv
B=$HOME/.cache/tgt-av1muchunkluma/debug/examples/dump_yuv
S=$HOME/.cache/av1muchunk422b
for n in c3_grad128_comp c4_grad256_comp; do
  ffmpeg -v error -y -i $S/$n.obu -pix_fmt yuv422p -f rawvideo /tmp/$n.ref.yuv
  TMPDIR=$HOME/.cache/tmp $B $S/$n.obu /tmp/$n.ours      # per-frame, per-plane cmp
done
# the gates (both green on main; red with bd762046's hunk reverted)
EC_AV1_AOMDEC=$HOME/.cache/aom-oracle/build/aomdec EC_AV1_REQUIRE_AOMDEC=1 \
  EC_AV1_REQUIRE_FFMPEG=1 TMPDIR=$HOME/.cache/tmp CARGO_TARGET_DIR=$HOME/.cache/tgt-av1muchunkluma \
  cargo test -p ec-av1 --lib muchunk_iis_128
```

Pre-fix control: `~/.cache/tgt-av1muchunk422class/debug/examples/dump_yuv`
(the class lane's tip `dc2deff2`, i.e. the tree `lanes/av1muchunk422class.report.md`
measured); it reproduces both streams' first wrong samples exactly.

Re-encode check (this lane):
```
AOM=~/.cache/aom-oracle/build/aomenc
$AOM --codec=av1 --profile=2 --input-chroma-subsampling-x=1 --input-chroma-subsampling-y=0 \
  --passes=1 --end-usage=q --cq-level=40 --cpu-used=0 --threads=1 --row-mt=0 --sb-size=128 \
  --min-partition-size=128 --max-partition-size=128 --enable-rect-partitions=0 \
  --enable-ab-partitions=0 --enable-1to4-partitions=0 --enable-palette=0 --enable-intrabc=0 \
  --deltaq-mode=0 --enable-tx-size-search=0 --limit=6 --lag-in-frames=25 --auto-alt-ref=1 \
  --enable-global-motion=1 --enable-warped-motion=1 --enable-masked-comp=1 \
  --enable-diff-wtd-comp=1 --enable-onesided-comp=1 --obu -o out.obu grad128.y4m
```

Note on local runs: the class lane's `/tmp` writes died with
`Disk quota exceeded (os error 122)` on the 128x256 frames; this lane's compare
files are per-frame and small enough that `/tmp` held them, but a home-backed
`TMPDIR` is the safe default.
