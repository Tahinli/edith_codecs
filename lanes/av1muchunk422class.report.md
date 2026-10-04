# lane-av1muchunk422class — does the DC-sign fix close the class, or only the pin?

Base: `main` @ `1e1d7565`. Branch: `lane/av1muchunk422class`. No code is
changed by this lane: it is a measurement.

**Verdict: still diverges — the class is NOT closed.**
`i5_grad256_iis.obu` (4:2:2, 128x256, 128 root) still diverges from ffmpeg on
the current tree, its first wrong sample is **V chroma (row 190, col 36) of
shown frame 2** (V-plane index 12196, value 146 vs ffmpeg's 147), and its
decode is **byte-identical before and after the fix** (0/6 frames changed). Its
first entropy fork is the *same signature the fix targeted* —
`dc_sign_ctx = 1` vs the oracle's `0` — but on a TX_32X32 chroma unit (plane 1,
`bc=8 br=8`) of the **intra-in-inter `side > 64` mu-chunk walk** (site 8),
which the fix did not touch.

The narrower claim the pin supports — the *compound-arm* `side > 64` mu-chunk
walk — does hold: the two other compound-arm streams in the hunt are now
byte-exact on every shown frame (below).

## 1. The streams

The hunt's eight streams still exist at `~/.cache/av1muchunk422b/` (byte-equal
to the shas in `lanes/av1muchunk422b.report.md`), so **zero aomenc attempts were
spent** — every number below is from those bytes. Five distinct diverging
streams were tested (c1 and c7 are the same bytes), plus the pin as control.
All are 4:2:2 (`dump_yuv` parses subsampling 1/0), F25:1 (**not** rate=1),
`part128 split=0` forced roots, and reach a 128 root.

| stream | sha256 (first 16) | bytes | shape | site-2 (compound) | site-8 (intra-in-inter) | post-fix | first wrong sample |
|---|---|---|---|---|---|---|---|
| `c1_rot128_comp` | `4d2c5dd641e9c63c` | 19320 | 128x128 | **4** | 0 | **EXACT** | — |
| `c2_rot128_comp_lo` | `7743ae49e35ca1c1` | 29699 | 128x128 | **4** | 0 | **EXACT** | — |
| `c3_grad128_comp` | `e801ce5596c9ed72` | 34449 | 128x128 | 0 | 16 | diverges | Y (62, 6) f1 |
| `c4_grad256_comp` | `90232481d39afa53` | 68925 | 128x256 | 0 | 16 (+24 site-5) | diverges | Y (126, 3) f1 |
| `i5_grad256_iis` | `64794f278eee738d` | 667 | 128x256 | 0 | 24 | **diverges** | V (190, 36) f2 |
| `i8_tsrc128_iis` (the pin) | `9942bae279a60ba6` | 895 | 128x128 | 8 | 0 | EXACT | — |
| `i6_grad128_iis` (pinned intra) | `4676fbc16e0f9459` | 391 | 128x128 | 0 | 4 | EXACT (control) | — |

`site-2/8` columns are the hunt's own env-gated probe counts
(`~/.cache/av1muchunk422b/*.probe`), not a re-derivation. Note the four streams
the `av1muchunk422b` report's "same shape on all four compound streams" sentence
refers to are **c1, c2, c7(=c1) and the pin** — of those, only c1/c2 are other
than the pin, and both are now exact.

## 2. Per-plane compare against ffmpeg, current main

`ffmpeg -v error -i X.obu -pix_fmt yuv422p -f rawvideo` vs
`dump_yuv X.obu <out>` (per-frame `cmp`), differing samples per shown frame
(Y / U / V):

| stream | f0 | f1 | f2 | f3 | f4 | f5 |
|---|---|---|---|---|---|---|
| `c1_rot128_comp` pre | 0 | 0 | U1070 V1793 | U2057 V1969 | U1081 V1829 | Y7977 U3994 V5127 |
| `c1_rot128_comp` **post** | 0 | 0 | 0 | 0 | 0 | 0 |
| `c2_rot128_comp_lo` pre | 0 | 0 | U1010 V1635 | Y4155 U3094 V3546 | Y5870 U3489 V4505 | Y6466 U3922 V5883 |
| `c2_rot128_comp_lo` **post** | 0 | 0 | 0 | 0 | 0 | 0 |
| `c3_grad128_comp` pre = post | 0 | Y8145 U5088 V6079 | Y8168 U5107 V6131 | Y16190 U8090 V8120 | Y16220 U8117 V8113 | 0 |
| `c4_grad256_comp` pre | 0 | Y16275 U8151 V8134 | Y32574 U16280 V16273 | Y32472 U16229 V16243 | Y32392 U16270 V16254 | Y24369 U13204 V14233 |
| `c4_grad256_comp` post | 0 | Y16280 U8143 V8138 | Y32747 U16379 V16329 | Y32480 U16224 V16233 | Y32448 U16252 V16241 | Y24415 U13212 V14250 |
| `i5_grad256_iis` pre = post | 0 | 0 | V2105 | 0 | V1982 | Y24155 U12162 V12091 |
| `i8_tsrc128_iis` pre | 0 | V2063 | V2138 | V2105 | V1988 | 0 |
| `i8_tsrc128_iis` post | 0 | 0 | 0 | 0 | 0 | 0 |

`cmp` of the pre-fix and post-fix dumps: c1 4/6 frames moved, c2 4/6,
c4 5/6; **i5 0/6 and c3 0/6 — the fix is inert on both**.

So the fix's measured effect on this set: c1 and c2 closed, the pin closed,
c4's residual altered, i5 and c3 untouched.

## 3. The still-diverging stream: i5, first wrong symbol measured

`i5` is the interesting one because its *pixels* carry the class signature —
Y and U exact, V differing (2105 samples, f2) — yet it never enters the
compound arm (0 site-2 hits; 24 site-8 hits). Paired our
`EC_TRACE_COEFF=1 EC_TRACE_COEFF_FRAME=1 EC_ECDUMP_IN=1` ladder against the
instrumented `~/.cache/aom-oracle/build/aomdec`'s
(`EC_TRACE_COEFF=1 EC_ECDUMP_IN=1`), pairing by read order and checking the
`ECIN=(value, rng)` anchor per unit: the first 48 units of **decode frame 1**
match exactly, then:

| | ours | oracle |
|---|---|---|
| unit (decode frame 1, order index 47) | chroma, `side=32` | plane 1, `bc=8 br=8 tx=3` (TX_32X32) |
| symbol | `tag=sign c=0` | `tag=sign c=0` |
| value | `sign=1` | `sign=1` |
| **row** | **`dc_sign_ctx = 1`** | **`dc_sign_ctx = 0`** |
| `rng` after | `55188` | `42728` |

Every symbol up to and including the sign VALUE is identical; the two decoders
read that sign from different `dc_sign_cdf` rows, so the entropy state forks
there — the next unit's `ECIN` already differs (`30384,55188` vs `30384,42728`).
That is byte-for-byte the `av1muchunk422sym` fork shape (ours `dcctx=1`, oracle
`0`, same value, different row, rng diverging), on a different walk.

The walk is identified by source and by the hunt's probe, not guessed: i5's
probe output is `24 MUCHUNK422 site=intra_in_inter` and **no**
`compound/singleref` line, and the intra-in-inter `side > 64` unit loop still
gathers its chroma context with the un-pair-ruled helper
(`crates/ec-av1/src/decode.rs:46820`,
`neighbours.around_mi(unit_mi, unit_luma_w)`, fed to
`dc_sign_ctx(around.2)` via `read_plane` at `decode.rs:22761`). At ss (1, 0)
that sums `unit_luma_w / MI` above cells where libaom sums
`txb_w_unit` = half as many chroma cells — the doubled above vote that flips the
signum. The sym lane's §6 residue enumerated the 46 `around_mi_rect` sites; this
call site is a bare `around_mi`, which that enumeration could not see.

**Not claimed:** that i5's first wrong *pixel* is produced by that fork unit.
The fork is named per the report's own rule ("first wrong symbol"); the first
wrong *sample* is V (190, 36) of shown frame 2, measured directly against
ffmpeg. The two are reported separately rather than stitched.

## 4. Why c3/c4 are not the same statement

Both are luma-first (c3 `Y(62,6)` f1, c4 `Y(126,3)` f1) and their diff counts
are whole-plane, two orders of magnitude above the class's V-band signature.
c3's decode is byte-identical before and after the fix, so its defect is not
this one. c4's dumps move with the fix (5/6 frames) — it reaches the
single-reference twin (site 5, 24 hits) that the fix also patched — but it
already diverged massively pre-fix and still does; whatever else is wrong with
it is not closed by this fix. Neither is claimed as the DC-sign class.

## 5. What this means for the pin's inference

The pin (`422_muchunk_compound_128root.obu`) demonstrates the fix on one
stream. Two more compound-arm streams (c1, c2) confirm it — but the class of
"4:2:2 128-root chroma context doubled by a luma-extent gather" is **not**
closed: the intra-in-inter `side > 64` walk still carries the identical
`dc_sign_ctx` fork (i5), and that walk is reachable on an existing,
non-regenerated stream. Any "class closed" statement needs to be scoped to the
compound arm, or the site-8 gather fixed and re-measured.

## 6. Repro

```
git worktree add -b lane/av1muchunk422class ~/.cache/wt/av1muchunk422class 1e1d7565
cd ~/.cache/wt/av1muchunk422class
CARGO_TARGET_DIR=$HOME/.cache/tgt-av1muchunk422class cargo build -p ec-av1 --example dump_yuv
B=$HOME/.cache/tgt-av1muchunk422class/debug/examples/dump_yuv
S=~/.cache/av1muchunk422b
for n in c1_rot128_comp c2_rot128_comp_lo c3_grad128_comp c4_grad256_comp i5_grad256_iis; do
  ffmpeg -v error -y -i $S/$n.obu -pix_fmt yuv422p -f rawvideo /tmp/$n.ref.yuv
  $B $S/$n.obu /tmp/$n.ours
done                                  # per-frame, per-plane cmp against /tmp/$n.ref.yuv
```

Pre-fix control: the same binary built at `df681fb7` (the sym fix's parent) in
`~/.cache/wt/prefix422class`.

Trace fork on i5 (see §3):

```
TMPDIR=$HOME/.cache/tmp EC_TRACE_COEFF=1 EC_TRACE_COEFF_FRAME=1 EC_ECDUMP_IN=1 \
  EC_DCDUMP=1 $B $S/i5_grad256_iis.obu /tmp/i5ours 2>ours.trace
TMPDIR=$HOME/.cache/tmp EC_TRACE_COEFF=1 EC_ECDUMP_IN=1 EC_ECDUMP=1 \
  ~/.cache/aom-oracle/build/aomdec --codec=av1 -o /tmp/o.y4m $S/i5_grad256_iis.obu 2>oracle.trace
# pair units in order, check ECIN=(value,rng): equal through index 47, fork at its sign read
```

Note on local runs: the first attempt wrote dump outputs under `/tmp` and died
with `Disk quota exceeded (os error 122)` on the 128x256 frames; every number
above is from the re-run with outputs and `TMPDIR` under `$HOME`.
