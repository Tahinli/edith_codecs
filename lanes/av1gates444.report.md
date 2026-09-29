# lane-av1gates444 — three measured-EXACT 4:4:4 lossy cells, pinned

## 1. What this lane is

Tests only. `crates/ec-av1/src/decode.rs` is **not** edited in the committed
tree (`git diff --stat` on the lane commit: `crates/ec-av1/src/stream.rs | 225
insertions(+), 0 deletions`). No new `#[ignore]`. No new `tests/*.rs` file —
all three gates live in the existing `ec-av1` lib test binary
(`crates/ec-av1/src/stream.rs`, `pub(crate) mod tests`).

Branch `lane-av1gates444`, one commit `81f88fd1` on top of `a21f3680`.

## 2. The cells, and the recipe that reproduces them

The three cells come from `lanes/av1formatsweep.report.md` §6.2 and
`lanes/av1444rect.report.md` §8. All three were **EXACT 4/4 with no committed
gate**; only the lane report carried the evidence.

The sweep's encode is reproduced byte-for-byte. The stream sizes and sha256
prefixes below match the sweep's table exactly, which is how the recipe is
confirmed rather than approximated:

```text
ffmpeg -f lavfi -i "testsrc2=size=<W>x<H>:rate=25" -frames:v 4 \
       -pix_fmt yuv444p -f yuv4mpegpipe - | \
aomenc --codec=av1 --profile=1 --passes=1 --end-usage=q --cq-level=20 \
       --cpu-used=2 --threads=1 --row-mt=0 --lag-in-frames=0 \
       --kf-max-dist=100 --limit=4 --obu -o - -
```

| cell | gate | bytes | sha256 | sweep said |
|---|---|---|---|---|
| odd 66x66 | `a_444_lossy_odd_66x66_stream_decodes_pixel_exact` | 5938 | `973eddf4…` | 5938 / `973eddf4…` |
| odd 130x122 | `a_444_lossy_odd_130x122_stream_decodes_pixel_exact` | 8945 | `c87ac65b…` | 8945 / `c87ac65b…` |
| 256x128 | `a_444_lossy_256x128_stream_decodes_pixel_exact` | 15239 | `06621606…` | 15239 / `06621606…` |

Two flags are load-bearing and both are documented in the gate's doc comment:

- `rate=25` — testsrc2's pattern is time-parameterised, so `rate=1` is a
  different stream.
- `--limit=4` — the sweep's frame count. At `--limit=6` the same recipe emits
  7674 / 11465 / 19121 bytes, i.e. a *different* stream than the one measured.
  Measured, not assumed.
- `--profile=1` is what makes the source 4:4:4 at all.

### 2.1 The third cell is NOT a superres cell

The sweep recorded it as "4:4:4 **superres** 256x128 (`--superres-mode=1`)" and
`av1444rect` §8 corrected that. Measured here to settle it independently:

| encode | bytes | sha256 |
|---|---|---|
| 256x128 without `--superres-mode=1` | 15239 | `881ab784…` at limit=6 / `06621606…` at limit=4 |
| 256x128 **with** `--superres-mode=1` | 15239 | identical |

The flag is a no-op on this aomenc build. The gate is therefore named
`a_444_lossy_256x128_stream_decodes_pixel_exact` — for its geometry — and the
doc comment says so explicitly, so the next lane does not re-derive a superres
claim from the sweep's table.

## 3. The three gates

All three share two helpers in the same file: `encode_444_lossy_live` (ffmpeg
y4m → live aomenc, `run_with_stdin` deadline, non-empty-OBU assert) and
`gate_444_lossy_live_exact` (the body). Modelled on
`a_444_lossy_rect4_inter_stream_decodes_pixel_exact` (stream.rs:7240), with
three deliberate differences:

1. **No pin.** The stream is encoded live against the oracle aomenc, because
   the claim is a property of the decoder at a geometry, not of a blob. The
   sweep's byte counts live in the doc comment as the recipe's provenance.
2. **The pixel compare cannot be skipped.** The old gate guards it with
   `if aomdec_path().is_file()`; these three assert. If aomenc resolved but
   no aomdec sits beside it, the gate FAILS rather than reporting green for a
   decoder it never looked at (class `gate-blind-to-the-arm`). It still SKIPs
   when there is no oracle at all, which `have_aomenc()` turns into a hard
   failure under `EC_AV1_REQUIRE_AOMENC=1` — the crate's existing convention.
3. **Per-cell geometry, asserted before the compare.** Frame count 4, and per
   frame `(f.width, f.height) == (W, H)` plus `f.u.len() == f.v.len() ==
   f.y.len() == W*H`. At 4:4:4 a decoder keeping a subsampled extent would
   hand quarter-size planes to the byte compare; the gate says which invariant
   broke instead of letting the compare stand in for the shape claim.

Non-vacuity, per cell: `decode::rect4_inter_own_chroma444_hits() > 0` after a
reset — the 4:4:4-only own-chroma-extent route in `decode_inter_block`'s
`around_c` that this class is about. A 4:2:0 stream of the same content reads
0. Measured **13 / 12 / 24** own-extent gathers at 66x66 / 130x122 / 256x128.

## 4. Green (this tree, oracle aomenc + aomdec present)

```
$ ec_av1 a_444_lossy_odd_66x66_stream_decodes_pixel_exact --nocapture
a_444_lossy_odd_66x66_stream_decodes_pixel_exact: 66x66 4:4:4 lossy, 4 decode-order frame(s)
  byte-exact vs aomdec (0 hidden), 13 own-extent 1:4 chroma gather(s)
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 714 filtered out

$ ec_av1 a_444_lossy_odd_130x122_stream_decodes_pixel_exact --nocapture
a_444_lossy_odd_130x122_stream_decodes_pixel_exact: 130x122 4:4:4 lossy, 4 decode-order frame(s)
  byte-exact vs aomdec (0 hidden), 12 own-extent 1:4 chroma gather(s)
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 714 filtered out

$ ec_av1 a_444_lossy_256x128_stream_decodes_pixel_exact --nocapture
a_444_lossy_256x128_stream_decodes_pixel_exact: 256x128 4:4:4 lossy, 4 decode-order frame(s)
  byte-exact vs aomdec (0 hidden), 24 own-extent 1:4 chroma gather(s)
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 714 filtered out
```

Untouched-and-still-green, same binary:

```
$ ec_av1 a_444_lossy_rect4_inter --test-threads=1     → ok
$ ec_av1 unrefreshed_primary_ref_slot --test-threads=1 → ok   (the stream.rs source-scan guard)
```

## 5. Red-before, measured (both mutations reverted, neither committed)

### 5.1 `decode.rs` `around_c` own-extent arm → the 4:2:0 pair extent

`Some(_) if ss_x(fctx) == 0 => { hit!(…); around }` replaced with the pair
gather `neighbours.around_mi_rect(s.pair_mi, pw, ph)` at `(16,8)` HORZ /
`(8,16)` VERT. The `hit!` stayed, so the reach assert still fires and the red
lands on the pixel arm rather than the counter — the lesson from
skill://ec-av1-pipeline-gate-counters.

| gate | result |
|---|---|
| 66x66 | **green** — expected, see 5.3 |
| 130x122 | **RED**: decode-order frame 3 of 4, byte 2208 (ours 134 vs oracle 170), **11521 bytes differ** |
| 256x128 | **RED**: decode-order frame 2 of 4, byte 192 (ours 92 vs oracle 106), **25296 bytes differ** |

The 130x122 number is the sweep's, to the byte: sweep §6.2 said "DIVERGENT
from f3, 11521 samples, first (f3, s2208) = Y(128,16)". The 256x128 numbers are
`av1444rect` §8's base row ("DIVERGENT f2 25296").

### 5.2 `ec-av1-syntax` `compute_image_size` — the mi-grid ceil floored

`h.mi_cols = 2 * ((h.frame_width + 7) >> 3)` →
`2 * (h.frame_width >> 3)` (and the row line likewise).

| gate | result |
|---|---|
| 66x66 | **RED**: `restoration.rs:783` — "range end index 4098 out of range for slice of length 4096" |
| 130x122 | **RED**: decode-order frame 0 of 4, byte 128 (ours 74 vs oracle 170), 22724 bytes differ |
| 256x128 | **green**, correctly — 256x128 is a multiple of 8 in both axes, so the ceil and the floor agree and the cell is not supposed to see this mutation |

### 5.3 What 66x66 is, stated honestly

`odd444_66x66` is a **coverage pin, not a witness for the strip fix**. The
sweep measured it EXACT 4/4 on the *pre-fix* base, so mutation 5.1 cannot
make it red and no mutation of the strip route ever will. Its
load-bearingness is mutation 5.2: it is the only gate here whose geometry is
partial in **both** axes, and dropping the mode-info grid's ceil breaks it on
the first decode. 130x122 and 256x128 are witnesses for the 4:4:4 lossy
strip-own-extent class; 66x66 is the pin for the odd-in-both-axes 4:4:4 lossy
walk. Both roles are load-bearing and neither is invented.

## 6. Invariants held

- `crates/ec-av1/src/decode.rs`: not edited in the committed tree.
- Existing gates: not edited — the commit is 225 insertions, 0 deletions, one
  file.
- Same test binary: `crates/ec-av1/src/stream.rs`, `pub(crate) mod tests`.
- No new `#[ignore]`.
- Commit message carries the mechanism and the measurements, no host paths.

## 7. Not done, stated

- The three gates encode on every run, so each costs one ffmpeg + one aomenc
  (~0.6 s measured, all three together). If a future lane finds that too
  expensive for a loop run, the pin migration is mechanical: the byte counts
  and sha256 prefixes are in §2.
- Only 8-bit lossy cq-20 is covered. 4:4:4 lossy at 10/12 bits and the tile
  cells stay the sweep's open list; nothing here narrows that gap.
