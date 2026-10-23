# lane av1e422edge: 4:2:2 chroma right-edge defect (D1 of av1chroma)

Base: main `2ccef013`. Worktree `~/.cache/wt/av1e422edge`, branch `lane/av1e422edge`.

## Defect

8-bit AV1 4:2:2, CDEF and LR off, 352x288: chroma diverged from libdav1d at
x=168..175 (the last 8 chroma columns), first at frame 3, row 72.

## Root cause

`crates/ec-av1/src/decode/leaf_sub8.rs`, the 4:2:2 sub-8x8 chroma reader. It
built the intra reach with `tx_w = 4 << ss_x` and `tx_h = 4 << ss_y`, which are
already in luma samples, then passed the frame-extent test as
`px + (tx_w << ss_x) < y.width` and `py + (tx_h << ss_y) < y.height`. The shift
was applied twice. At `ss_x = 1`, a 4-wide chroma unit at luma x 336 of a
352-wide frame tested `336 + 16 < 352`, which is false, so its above-right
samples were dropped and replicated. The correct test is `336 + 8 < 352`, which
is true. libaom's `right_available` is `mi_col + ((col_off + txw) << ss_x) <
mi_col_end`, which is the same test with one shift. The spec's BlockDecoded
rule for the above-right block gives the same answer.

Measured on frame 3, unit (168,72), U and V, mode D45 with the edge filter on
(strength 1). The real above-right samples x=172..175 reproduce libaom's output
exactly (row 0 `[160,162,162,158]`). The replicated ones give ours
`[160,162,164,163]`. The reach trace showed `ar=false` for this unit with
`above_avail=false`, although the unit is not on the frame edge.

Fix (`leaf_sub8.rs`, the `Reach::of_tu_chroma` call): drop the extra shifts, so
the tests are `py > 0 && px + tx_w < y.width` and `px > 0 && py + tx_h <
y.height`. This is the only call site of `of_tu_chroma`, and the reader is
4:2:2-only, so 4:2:0 and 4:4:4 are not touched.

## Test

`crates/ec-av1/tests/yuv422_dav1d.rs` (new). It encodes with ffmpeg
`libaom-av1` (mandelbrot plus noise, `-lag-in-frames 0 -crf 25`), with
`-aom-params enable-cdef=0:enable-restoration=0` for the off cases. It decodes
with `ec_av1::stream::decode_stream` and with ffmpeg `libdav1d` rawvideo, and
compares every shown frame and plane. It SKIPs when ffmpeg lacks `libaom-av1`
or `libdav1d`.

Cases: 352x288 8-bit CDEF/LR off and on, plus 176x96, 200x120 and 330x190 at
8 and 10 bit, each with CDEF/LR off and on. 14 cases, 8 frames each.

Before the fix: FAIL. 352x288 8-bit off showed U and V diffs from frame 3, and
352x288 8-bit on showed diffs from frame 4. The other cases passed.
After the fix: `test yuv422_matches_dav1d_byte_exact ... ok`, all 14 cases
byte-exact.

## Verify commands

- `scripts/remote-run.sh av1e422edge --host b -- cargo test --release -p ec-av1 --test yuv422_dav1d` (before: FAILED; after: ok, 217 s)
- `EC_AV1_AOM_DIR=$HOME/fixtures/av1-aom EC_REQUIRE_FIXTURES=1 cargo test --release -p ec-av1 --test aom_conformance` on host c: `242/242 exact (611 shown frames), 0 refused by name, 0 known failing, 0 failed`
- `EC_MEMGUARD_MAX=6G cargo test --release -p ec-av1 --no-fail-fast` on host b: 825 passed, 0 failed (the lib binary was SIGKILLed by the default 3500M memguard on the first attempt, so it was re-run with 6G).
- `cargo clippy -p ec-av1 --all-targets -- -D warnings` on host b: exit 0.

## Not done

- The `Reach::of` square variant (`x + side < width`) is not changed. It uses the same padded width and may have an odd-width edge case. Not covered by this lane.
- The CDEF-on case was failing before the fix and is exact now, but it has not been checked on the 4:2:2 LR-on path separately from the 8-frame clip.
- Not verified: whether the h242map 4:2:2 height sweep (`R422_320x*`) is explained by this same defect. It was not re-run here.
