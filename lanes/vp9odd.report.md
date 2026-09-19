# Lane vp9odd: VP9 odd coded extents (chroma ceils at (w+1)/2)

Branch `lane-vp9-odd`, base `main e910d25b`. Crate scope: `crates/ec-vp9`.
Worktree `/home/tahinli/Documents/Code/Rust/edith_codecs-vp9odd`, private
`CARGO_TARGET_DIR=$HOME/.cache/cargo-target-vp9odd`.

Deliverable: the named `vp9 inter odd frame dimensions` refusal is removed and
odd-width / odd-height streams decode byte-identically to a libvpx 1.15.0
oracle. Root cause: the decoder stored/cropped 4:2:0 chroma at `floor(w/2)`
while libvpx stores and crops at `uv_crop_width = (w+1)/2` (CEIL). Unifying on
ceil in the inter predictor's reference extent and in `crop_picture` fixes it.

## Baseline (milestone 0)

    $ CARGO_TARGET_DIR=$HOME/.cache/cargo-target-vp9odd cargo test -p ec-vp9
    20 test binaries, 0 failed, 0 ignored   (pre-fix; 21 after the new witness)

Pre-existing warnings (unchanged, present at base): `modes.rs` unused
`partition_probs` / `x_mis` / `y_mis` + `read_partition`, `tables/mod.rs`
`TM_PRED` (5).

## Fail-pre-fix witness (milestone 1)

The refusal fires on any odd coded extent once a reference slot already holds
an odd-sized frame. `$HOME/.cache/vp9pix/refusal_odd.ivf` (patched keyframe +
an inter frame that inherits the odd size) reproduces it:

    $ CARGO_TARGET_DIR=$HOME/.cache/cargo-target-vp9odd \
        INTER_IVF=$HOME/.cache/vp9pix/refusal_odd.ivf \
        EC_VP9_PIXDUMP=/tmp/our_ro.pix \
        cargo test -q -p ec-vp9 --test scratch_pixdump -- --nocapture
    PIXFRAME input=0 shown=0 322x242 321x241
    PIXERR input=1: unsupported: vp9 inter odd frame dimensions (321x241: the
      reference chroma planes of an odd-sized frame are stored at floor(w/2)
      but the predictor reads libvpx's uv_crop_width = (w+1)/2)

After the fix the named ODD-DIMS refusal is gone. This particular fixture does
not then decode cleanly, because its frame 1 is genuinely corrupt: libvpx itself
reports `Corrupt frame detected` on it (see Deferred). So the fixture witnesses
only the removal of the refusal; the permanent odd witness generates its own
valid odd stream instead.

## Root cause

- inter predictor (`decode.rs::inter_predict_plane`): `RefPlane.width/height`
  used `coded >> s` for chroma -> `floor(w/2)`. libvpx's border/inside test
  uses `uv_crop_width = (w+1)/2`, so the last chroma column/row was treated as
  outside the frame and border-clipped.
- returned picture (`decode.rs::crop_picture`): chroma cropped + `uv_stride`
  at `w/2` / `h/2` -> a floor dump that is one column/row short of libvpx's
  `vpx_image` (which carries `ceil(d_w/2) x ceil(d_h/2)`).
- `Planes::dims` (the internal buffer stride) is unaffected: the scratch/reference
  buffers are 64-aligned, so `aw/2` already equals `ceil` and covers the extra
  sample. Only the *readable/cropped* extents were wrong.

## Change (milestone 2)

`crates/ec-vp9/src/decode.rs` only:

- `inter_predict_plane`: `width: (ref_frame.width as usize + s) >> s`,
  `height: (ref_frame.height as usize + s) >> s` (luma `s = 0` unchanged,
  chroma ceil).
- `crop_picture`: `let (cw, chh) = ((w + 1) / 2, (h + 1) / 2)` for U/V crop
  and `uv_stride`.
- delete the `hdr.width % 2 != 0 || hdr.height % 2 != 0` refusal block and its
  module-doc sentence.

No mi-grid or bitstream change: the coded size only affects plane storage/crop,
so the tile syntax is untouched. The loop filter keeps its existing MI-aligned
extents (`vw = mi_cols*8`, `uv_h = vh/2`) and the unbounded chroma column groups;
an odd stream writes the same overhang the even path already did.

## Byte-exactness (milestone 3)

Oracle: `$HOME/.cache/vp9pix/drv_pix2` (new), a libvpx pixel dumper that uses
the CODED size (`img->d_w/d_h`) and the vpx_image chroma convention
(`ceil(w/2) x ceil(h/2)`). It is byte-identical to the frozen oracle on the
even 1080p entry stream:

    $ PIXDUMP=/tmp/o1_frozen.pix $HOME/.cache/vp9pix/drv_pix inter1.ivf
    $ PIXDUMP=/tmp/o1_new.pix    $HOME/.cache/vp9pix/drv_pix2 inter1.ivf
    $ cmp /tmp/o1_frozen.pix /tmp/o1_new.pix    # identical

and ffmpeg's rawvideo output equals it on an odd stream too, so ffmpeg is a
valid odd oracle (used by the in-repo witness).

Comparator: `$HOME/.cache/vp9pix/pixcmp2.py <oracle> <ours> <w> <h>` (ceil
chroma; exact for even and odd). Sweep driver: `sweep2.sh <worktree> <stream>
<w> <h>` (oracle dump + our dump + `pixcmp2.py`).

### Odd stream generation

libvpx-vp9's encoder rounds an odd source through its scaler, so odd fixtures
are an even `322x242` (or `1920x1080`, `320x240`) encode whose keyframe's
uncompressed-header size fields are patched to the odd target and whose IVF
container size is rewritten:

    $ ffmpeg -v error -y -f lavfi -i testsrc2=size=322x242:rate=24:duration=0.5 \
        -c:v libvpx-vp9 -g 999 -auto-alt-ref 0 -crf 30 -deadline good \
        -cpu-used 4 -pix_fmt yuv420p -f ivf /tmp/src322g.ivf
    $ python3 $HOME/.cache/vp9pix/patch_kf_size.py /tmp/src322g.ivf \
        /tmp/odd_321x241.ivf 321 241

The patch flips the 16-bit `width-1` / `height-1` fields at bit 36 of the
keyframe (profile 0, 8-bit, 4:2:0, `error_resilient = 0`: 8 header bits +
24-bit sync + 3 `color_space` + 1 `color_range`). 322<->321 and 242<->241 are
mi-grid invariant (both ceil to 41x31 mi units), so the tile data stays valid
and every later frame inherits the odd size through `found_ref`.

### Results (all frames, loop filter ON)

| stream | coded | frames | result |
|---|---|---|---|
| testsrc2 patched | 321x241 | 12 | **all byte-identical** |
| testsrc2 patched | 321x242 (odd width) | 12 | **all byte-identical** |
| testsrc2 patched | 322x241 (odd height) | 12 | **all byte-identical** |
| testsrc2 patched, `-tile-columns 1` | 321x241 | 12 | **all byte-identical** |
| real 1080p corpus patched | 1919x1079 | 48 | **all byte-identical** |
| real altref corpus patched | 319x239 | 60 | **all byte-identical** |

Real-content odd witnesses: `vp9-1080p-23.976-8bit.ivf` (single keyframe) and
`vp9-superframe-altref.ivf` patched at frame 0, so the whole stream is odd.

Loop filter is active on the odd streams (verified: `EC_VP9_SKIP_LF=1` changes
the output of `odd_321x241`, so the odd chroma/row extents pass through the
filter byte-exactly).

### Even corpus re-sweep (no regression)

| stream | frames | result |
|---|---|---|
| `inter1.ivf` 1080p | 3 | **all byte-identical** |
| `inter.ivf` 1080p (2 tile cols) | 3 | **all byte-identical** |
| `vp9-1080p-23.976-8bit.ivf` | 48 | **all byte-identical** |
| `vp9-2160p-23.976-8bit.ivf` | 48 | **all byte-identical** |
| `vp9-superframe-altref.ivf` | 60 | **all byte-identical** |

## Regression witness (milestone 4)

`crates/ec-vp9/tests/odd_dimensions_exact.rs` (3 tests: odd width, odd height,
both). It generates the even `322x242` GOP with ffmpeg, patches the keyframe
size fields, decodes with our decoder and compares every shown frame byte-for-
byte against ffmpeg's libvpx rawvideo, asserting the coded size and the ceil
chroma length/stride on every frame.

    $ CARGO_TARGET_DIR=$HOME/.cache/cargo-target-vp9odd \
        cargo test -p ec-vp9 --test odd_dimensions_exact
    test result: ok. 3 passed; 0 failed

Non-vacuity (temporarily restoring the floor layout: `>> s` and `w/2, h/2`):

    odd_width_matches_ffmpeg            FAILED
    odd_height_matches_ffmpeg           FAILED
    odd_width_and_height_matches_ffmpeg FAILED
    (panic at odd_dimensions_exact.rs:132, the ceil chroma length assert)
    inter_pixels_exact (even)           2 passed   # even path unaffected

Fixed layout: the 3 odd tests pass and the even tests still pass.

## Caller-migration census (milestone 5)

The returned plane layout is `ec_vp9::decode::Picture`. Census of every
in-workspace reader:

- **Dependency edges**: no crate declares a dependency on `ec-vp9`. The only
  edge is `ec-hw -> ec-vp9-syntax` (untouched). `tools/` (`ec-bench`, `oracle`)
  does not reference `ec-vp9`. No `ec_vp9::` path is used outside
  `crates/ec-vp9/`.
- **Inside the crate**: `crop_picture` is the sole producer of `Picture`;
  readers are the tests. `keyframe_exact.rs` / `inter_pixels_exact.rs` use
  even dims (`(w/2)*(h/2)` == ceil) and are unaffected;
  `scratch_realsweep.rs` only prints `pic.uv_stride`; `scratch_pixdump.rs`
  dumps `pic.{y,u,v}` and is now ceil-correct by construction.

Conclusion: zero external consumers; no shim or dual layout needed. The only
layout change is `Picture.uv_stride`/chroma length for odd coded extents.

## Gates

    $ CARGO_TARGET_DIR=$HOME/.cache/cargo-target-vp9odd cargo test -p ec-vp9
    exit=0 - 21 test binaries, 0 failed, 0 ignored (baseline 20; +1 witness)

Warning parity: 5 pre-existing warnings, none added, none from the new file.

## Deferred / not touched

- **frame-72 desync on `vp9-1080p-60-8bit.ivf`** — owned by the sibling lane;
  not touched here. It is even-dimension and orthogonal to this change.
- `$HOME/.cache/vp9pix/refusal_odd.ivf` is no longer a valid refusal witness:
  libvpx itself reports `Corrupt frame detected` on its frame 1, so it was a
  refusal-only probe. The permanent witness now generates its own odd stream.
- No resync/`show_existing_frame`-specific odd test; the odd path reuses the
  same `crop_picture` used by the show-existing return, covered indirectly by
  the sweeps.

## Repro (all commands)

    # build + suite
    cd /home/tahinli/Documents/Code/Rust/edith_codecs-vp9odd
    CARGO_TARGET_DIR=$HOME/.cache/cargo-target-vp9odd cargo test -p ec-vp9

    # odd witness (self-generating)
    CARGO_TARGET_DIR=$HOME/.cache/cargo-target-vp9odd cargo test -p ec-vp9 \
        --test odd_dimensions_exact

    # odd real-content sweep (any stream)
    $HOME/.cache/vp9pix/sweep2.sh $PWD /tmp/rc_1919x1079.ivf 1919 1079

    # rebuild the odd-aware oracle if needed
    gcc -O2 -I $HOME/.cache/vp9pix/libvpx-hdr/src \
        $HOME/.cache/vp9pix/drv_pix2.c -o $HOME/.cache/vp9pix/drv_pix2 \
        /usr/lib64/libvpx.so.9 -lm -lpthread

## VERDICT

GATE 1 - `cargo test -p ec-vp9` green (21 binaries, 0 failed, 0 ignored):
**PASS**.
GATE 2 - odd-width, odd-height and both decode byte-exactly vs libvpx 1.15.0,
and the full even corpus re-sweeps all-identical: **PASS**.
GATE 3 - the named `vp9 inter odd frame dimensions` refusal is gone (pre-fix
string captured above) and the fix is non-vacuous (floor layout fails the odd
witness): **PASS**.
GATE 4 - caller-migration census complete (zero external consumers):
**PASS**.
GATE 5 - committed to `lane-vp9-odd`, clean tree, no push: `e8152b9b`:
**PASS**.

## MERGE-SIDE FOLLOW-UPS

- Create-list: `crates/ec-vp9/src/decode.rs`,
  `crates/ec-vp9/tests/odd_dimensions_exact.rs`, this report. No other file.
- The `$HOME/.cache/vp9pix` assets added here (`drv_pix2.c`/`drv_pix2`,
  `pixcmp2.py`, `patch_kf_size.py`, `sweep2.sh`, `libvpx-hdr/`) are outside the
  repo; the in-repo witness is self-contained (ffmpeg + a pure-Rust patch).
- The odd-aware oracle links against the system libvpx 1.15.0
  (`/usr/lib64/libvpx.so.9`), byte-identical to the frozen oracle on even
  content; `libvpx-devel` is not installed, so the headers come from a
  `v1.15.0` source checkout.
