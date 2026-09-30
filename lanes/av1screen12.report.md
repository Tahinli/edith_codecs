# lane-av1screen12 -- the 12-bit `allow_screen_content_tools` refusal is BROADER than the gap it names

Base `2d8851c0`, branch `lane-av1screen12`.

## 1. The claim under test

`crates/ec-av1/src/stream.rs` (frame gate, was `:1842`) refused any frame with
`bit_depth == 12 && header.allow_screen_content_tools`, by the string

```
a 12-bit frame with screen content tools (allow_screen_content_tools=1: neither palette nor intrabc has a 12-bit witness)
```

`allow_screen_content_tools` is a single header bit that opens two things: the
`palette_y_mode`/`palette_uv_mode` symbols on an intra block, and the leaf
`use_intrabc` symbol. The string conflated "the frame sets the bit" with "a
block actually used a palette or intrabc", and asserted an encoder-side
production gap ("neither ... has a 12-bit witness") that this lane re-measured.

## 2. Reproduction on base (live)

Six live `aomenc --bit-depth=12` encodes, `testsrc2=s=160x128:r=25` and
`smptebars=s=160x128:r=25` x {screen-only, palette+intrabc, lossless+palette},
recipe `--profile=2 --passes=1 --end-usage=q --cq-level=20 --cpu-used=0
--threads=1 --row-mt=0 --sb-size=64 --lag-in-frames=0 --auto-alt-ref=0
--kf-max-dist=1000 --input-bit-depth=12 --bit-depth=12 --enable-cdef=1
--enable-restoration=1 --obu` plus `--tune-content=screen --enable-palette=1
--enable-intrabc=1` (+ `--lossless=1`).

BEFORE: all 6 refused, by that string.

Frame headers read back through this crate's own parser:

| stream | frames | `allow_screen_content_tools` | `allow_intrabc` |
| --- | --- | --- | --- |
| all six 12-bit arms | 2 | true | **false** |

8-BIT control with the same recipe: `allow_intrabc` also false.

## 3. What is actually unproducible -- and what is not

The "no producer" half was a RECIPE artefact, not an encoder limit.
`--tune-content=screen` does NOT decide that frame-header bit in this oracle
build; libaom's content detector does (`av1/encoder/encoder.c:2404`). Measured,
same source, with and without the flag:

| source (8-bit) | `allow_screen_content_tools` | `allow_intrabc` |
| --- | --- | --- |
| `smptebars=size=128x96` | true | **false** |
| `smptebars=size=128x96` + `-vf tile=2x2` (256x192) | true | **true** |

and the tiled shape gives `allow_intrabc = 1` at 8, 10 AND 12 bits -- it is
exactly the source the repo's own intrabc census
(`a_sub8_leaf_census_over_intrabc_screen_streams_measures_the_sub8_refusal`)
already uses. So the 12-bit intrabc half was always producible; only the
"simple recipe" hunt missed it.

## 4. After the lift, per arm

Guard removed (patch-run-restore: `if false && bit_depth == 12 && ...` during
measurement, deleted for the commit). Three arms, all measured against the
INSTRUMENTED `aomdec --rawvideo` **and** ffmpeg, 0 wrong samples:

| arm | pin | frames | screen frames | `allow_intrabc` frames | palette / palette-UV / intrabc blocks | wrong vs aomdec | wrong vs ffmpeg |
| --- | --- | --- | --- | --- | --- | --- | --- |
| palette | `screen12_palette.obu` | 2 | 2 | 0 | 76 / 43 / 0 | 0 | 0 |
| palette + intrabc | `screen12_intrabc.obu` | 1 | 1 | 1 | 36 / 34 / 11 | 0 | 0 |
| lossless + palette | `screen12_lossless_palette.obu` | 2 | 2 | 0 | 80 / 59 / 0 | 0 | 0 |

### Pins (sha256)

| pin | bytes | sha256 |
| --- | --- | --- |
| `crates/ec-av1/fixtures/screen12_palette.obu` | 3616 | `adfe26c67c08a7e986db2743b914ecc3e214c56dd2abbdfd17ebf99f06de64c8` |
| `crates/ec-av1/fixtures/screen12_intrabc.obu` | 482 | `a3b6b94cabdd09a12057c4688990d55495826b87a14332c386d319629070382e` |
| `crates/ec-av1/fixtures/screen12_lossless_palette.obu` | 11698 | `ec7942c96763593e6d8e4b3fb23c03930bba3bb743102d0cf552fe73ae11c203` |

### Recipes

* **palette** -- `ffmpeg -f lavfi -i testsrc2=s=160x128:r=25 -t 0.2 -pix_fmt
  yuv420p12le -strict -1 -f yuv4mpegpipe -` piped to `encode_12bit` with
  `--tune-content=screen --enable-palette=1 --enable-intrabc=1`, `--limit=2`.
* **palette + intrabc** -- `screen_intrabc_stream_at_depth("smptebars=size=128x96:rate=25", "30", "0", tiled, square_only=false, 12, ["--enable-palette=1"])`
  (`-vf tile=2x2` -> 256x192, `--tune-content=screen --enable-intrabc=1
  --enable-palette=0` + `--enable-palette=1` last, `--cq-level=30
  --enable-tx-size-search=0 --min-partition-size=8 --max-partition-size=32
  --sb-size=64 --kf-max-dist=1 --limit=1`): ONE 12-bit key frame.
* **lossless + palette** -- the palette recipe plus `--lossless=1`, so the frame
  is `CodedLossless` (WHT on every plane) AND screen-content.

Each gate re-runs its live recipe and asserts it reproduces the pin byte for
byte (length + `fnv1a64`), so the recipe above is not decoration.

## 5. The control (every claim carries one)

Each gate runs `count_rawvideo_diffs` twice on the SAME stream:

* clean: `(wrong_y, wrong_u, wrong_v, frames, exact) == (0, 0, 0, F, F)`;
* one bit flipped in the ORACLE's own frame-0 luma HIGH byte (index 1):
  `(1, 0, 0, F, F-1)` -- **exactly +1 wrong luma byte and exactly one fewer
  exact frame**. Index 1 is chosen because a 12-bit sample's high byte holds
  bits 8..15 only, so flipping its low bit can never carry: "+1 in the right
  plane and frame" is exact by construction, not hoped for.

A comparator that compared our samples with themselves would report 0 in both
arms (class `oracle-diff-counter-tautology`).

## 6. Mutation proof (gate bites)

`read_palette_colors_y` (`decode.rs`), one line:

```rust
-        let first = dec.literal(bd) as u16;
+        let first = (dec.literal(bd) as u16) & 0x00ff;   // 12-bit colours truncated to 8
```

Bit-preserving (the same number of bits is read, so no desync -- only palette
pixels are wrong). Result:
`a_real_aomenc_12bit_screen_content_palette_stream_decodes_pixel_exact` RED --
`119726 bytes of decoded output differ from aomdec (first at 1)`. Reverted; the
gate is green again.

## 7. Inventory and census

Both copies of the string in `crates/ec-av1/src/refusal_inventory.rs` were
removed -- the `REFUSALS` row and its `PROVEN` `(string, gate)` pair -- and
replaced by a comment recording the lift, the three witnesses and the
encoder-side measurement.

```
refusal inventory: 33 refusals + 1 capability claims, 33 proven   (before)
refusal inventory: 32 refusals + 1 capability claims, 32 proven   (after)
anchor strength: 25 rows quote the WHOLE refusal string, 7 quote its LEADING CLAUSE, 0 match neither
```

Green after the change:

* `cargo test -p ec-av1 --lib -- refus` -- 56 passed, 0 failed
* `cargo test -p ec-av1 --lib -- gate_coverage` -- 20 passed; `282 real-aomenc
  gates, 119 of them 10-bit`, `NEVER_EXERCISED_8BIT (0 of 26)`,
  `NEVER_EXERCISED_10BIT (0 of 26)`, and
  `every_pin_a_gate_reads_is_committed_under_the_crate` (the three new pins).

The removed gate `a_12bit_screen_content_stream_is_refused_by_name` had no other
referrer in `crates/`.

## 8. `not_done`

Nothing in the 12-bit screen-content surface still refuses. Named caveats:

1. **`--tune-content=screen` is a no-op for the `allow_intrabc` decision in this
   oracle build** and this lane did not chase libaom's override further than the
   measurement (with/without the flag, both readings identical at 128x96 and at
   256x192). The gates read the bit out of the header, so they cannot go
   vacuous on it, but a future libaom that honours the flag could move the
   256x192 recipe.
2. **12-bit `palette` on a rect HORZ/VERT intra strip and on an intra-in-inter
   block** is not separately witnessed at 12 bits; the arms here cover the
   square key-frame palette, the chroma palette, and intra-block copy. The
   rect/intra-in-inter palette syntaxes are depth-parameterised the same way
   (`read_palette_colors_*` at `bit_depth(fctx)`), so this is untested-cell, not
   a known-diverging cell. No claim is made for it.
3. **No 12-bit 4:4:4 screen-content arm**: the 12-bit 4:4:4 witness
   `a_444_12bit_inter_sequence_decodes_pixel_exact` uses smooth content that
   leaves `allow_screen_content_tools = 0`, and the recipe hunt for a 12-bit
   4:4:4 screen stream was not run in this lane.
