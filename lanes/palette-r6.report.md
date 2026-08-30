# lane-palette r6 report

VERDICT: PARTIAL -- the bisect the charter asked for is done and the root
cause is pinned exactly; the fix itself (reorder + UV palette mode_info
reads) is not implemented this round, so the refusal stays and no gate
was flipped to pixel-exact.

## The bisect (committed 7485651)

Added the matching `EC_AV1_TRACE`-gated `EC_PAL row=.. col=.. ctx=.. n=.. rng=..`
/ `EC_PAL_VAL row=.. col=.. color_idx=.. rng=..` print inside our own
`decode_color_index_map` (`crates/ec-av1/src/decode.rs` ~2370, matching
r4's oracle rung field-for-field), plus `TRACE pre_palette_y_mode` /
`palette_y_mode` / `palette_y_size` / `palette_y_colors` prints around
the Y palette-mode-info reads just above it (~3010-3024).

Regenerated the r3/r4 gate's exact fixture once (`ffmpeg smptebars
size=64x64:rate=25` piped through `hue=s=0`, then the gate's pinned
`aomenc` recipe -- byte-identical across two ffmpeg runs, saved to
`/tmp/.../scratchpad/palette/out.obu`, 112 bytes) and ran both traces
against the identical bytes:

- `aomdec EC_TRACE_PALETTE=1` (r4's existing rungs 6/6b)
- our own decoder via a throwaway `#[ignore]` test reading
  `EC_AV1_SCRATCH_FILE` and calling `decode_stream` with `EC_AV1_TRACE=1`
  (not committed -- removed after use, see below)

**Range compare, per `compare-range-not-tell`:** every symbol up through
the Y palette colours is bit-identical between the two decoders for the
frame's first (top-left, mi=(0,0)) block:

| point | oracle rng | ours rng |
|---|---|---|
| partition_w32(0,0) read | 58370 | 58370 |
| entering `read_palette_mode_info` (post skip/y_mode/uv_mode) | 34206 | 34206 |
| after `palette_y_mode` symbol (value=1) | 60032 | 60032 |
| after `palette_y_size` symbol (n=4) | 39344 | 39344 |
| after `read_palette_colors_y` (colours `[112,131,162,180]`) | 39432 | 39432 |
| `decode_color_map_tokens` / our `decode_color_index_map` **entry** | **51752** | **39432** |

Ranges match to the byte through the Y colour read, then diverge exactly
at the point our `decode_color_index_map` is called. **Per the charter's
own steer this rules out a table/context bug in the map reader itself**
(r3 already audited those tables and this round's range trace confirms
it end to end) **and confirms the bug is positional, not a read bug**.

## Root cause: `decode_color_index_map` is called from the wrong syntax position

Traced why the oracle's rng jumps from 39432 (after Y colours) to 51752
(entering the color-index-map) with no observable step in between on our
side: `libaom`'s `decode_color_map_tokens` is **not** called inline from
`read_palette_mode_info`. It runs later, from `parse_decode_block`
(`decodeframe.c:1135`, `av1_visit_palette(pbi, xd, r,
av1_decode_palette_tokens)`), called right after `decode_mbmi_block`
returns -- i.e. after the *entire* mode-info read for the block,
including **UV palette mode_info** (`read_palette_mode_info`'s own
second half: `palette_uv_mode_cdf` / `palette_uv_size_cdf` /
`read_palette_colors_uv`, gated on `uv_mode == UV_DC_PRED &&
is_chroma_ref`) and `read_filter_intra_mode_info`. Confirmed live by
patching a throwaway `EC_PRE_PALETTE` / `EC_PALETTE_Y_MODE` /
`EC_PALETTE_Y_SIZE` / `EC_PALETTE_Y_COLORS` print into the oracle's
`decodemv.c` (rebuilt aomdec, ran the trace, matched ours field-for-field
through `EC_PALETTE_Y_COLORS rng=39432`, then confirmed
`read_palette_mode_info` returns and control passes to
`av1_visit_palette` before `decode_color_map_tokens` runs) -- **reverted
this patch after use** (`git diff --stat` on `~/.cache/aom-oracle/src`
clean of it, `ninja aomdec` rebuilt back to the pre-patch binary) since
that oracle checkout is shared with sibling lanes and the charter forbids
touching sibling worktrees; this diagnostic lived entirely outside this
repo and is not committed here.

This gate's block has `uv_mode=0` (`UV_DC_PRED`) and is chroma-referenced
(32x32 luma with 4:2:0 subsampling is a real chroma block), so the
oracle genuinely reads a `palette_uv_mode` symbol (and, since it's a
binary CDF, possibly `palette_uv_size` + `read_palette_colors_uv` too)
between the Y colours and the color-index-map read -- bits our decoder
never consumes at all. Our `decode_color_index_map(dec, cdfs, n, side)`
call sits immediately after `read_palette_colors_y` inside the Y-palette
branch (`decode.rs` ~3023), which is wrong on two counts: (1) it skips
UV palette mode_info entirely when the block is chroma-referenced and
UV-DC, and (2) even where UV genuinely isn't read (monochrome / not
chroma-referenced / UV mode != DC), libaom still defers the color-index
map to a separate post-mode-info pass (after `filter_intra`), not inline.

## What this closes, what it doesn't

r3's table audit + this round's range trace together give a decisive
answer per `worker-cap-spent-reading`/`compare-range-not-tell`: **every
table this lane touches is exact**; the bug is 100% a call-site ordering
bug, not a CDF/context bug, so no more table re-reading is owed here.

**Not done this round (next round's job):** the actual reorder fix --
read (and, per the "Then" milestone, eventually reconstruct) UV palette
mode_info in `read_intra_mode`'s palette branch right after the Y
colours, then move both the Y and UV `decode_color_index_map` calls to
run after `filter_intra` is read, matching `parse_decode_block`'s real
call order. That is squarely the charter's "Then" milestone ("Palette
UV") arriving earlier than expected -- it turns out UV support isn't a
separate later feature, it's a hard prerequisite for Y's own bits to stay
synced whenever the block is chroma-referenced with UV_DC_PRED. The
refusal (`"a block that actually uses a palette (Y) -- the index map
decodes but the reconstructed pixels do not match libaom yet
(lane-palette r3/r4)"`) stays exactly as-is; removing it before the
reorder + UV-read fix lands would ship a real desync as a "decode",
which the charter explicitly forbids ("remove the refusal only in the
same commit as the gate that proves pixel exactness").

## Refusal strings

None added, none removed, none reworded this round.

## Check

`EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib -j4` (timeout
600000ms, ran ~122s): **244 passed, 0 failed, 17 ignored** -- unchanged
from r5's baseline. `cargo build -p ec-av1 --tests -j4`: clean, no new
warnings from the trace additions.

## Handoff for the next round

1. In `read_intra_mode`'s palette branch (`decode.rs` ~3010-3024), right
   after `read_palette_colors_y`, add the UV palette mode_info read:
   `if num_planes > 1 && uv_mode == UV_DC_PRED (0) && is_chroma_ref { read
   palette_uv_mode symbol; if set, palette_uv_size + palette colours (UV,
   two channels) }` -- needs a `palette_uv_mode_cdf` / `palette_uv_size_cdf`
   pair and a `read_palette_colors_uv` port (`decodemv.c` `read_palette_colors_uv`,
   not yet ported -- check `cdf.rs` for whether the UV CDF tables already
   exist before writing new ones). `is_chroma_ref` needs threading in if
   it isn't already available at this call site.
2. Move both the Y (`decode_color_index_map(dec, cdfs, n, side)`, luma
   plane) and the new UV color-index-map read to run *after*
   `filter_intra` is read, not inline in the palette branch -- matching
   `parse_decode_block`'s `decode_mbmi_block` (all mode info) then
   `av1_visit_palette` (both planes' index maps) order. This likely means
   restructuring `read_intra_mode`'s return type to carry the *undecoded*
   palette colours/size out, with the actual `decode_color_index_map`
   call(s) moved to the caller, after the `filter_intra` result is known.
3. Once reordered, rerun this round's exact trace recipe (`out.obu` in
   the scratchpad, or regenerate -- see r3/r4/r6 for the recipe) with both
   `EC_AV1_TRACE=1` on ours and (temporarily re-patching, then reverting,
   the same 3 oracle `decodemv.c` prints this round used) to confirm the
   ranges now match all the way to the end of the Y (and UV) color-index
   maps before flipping the gate from "refuses by name" back to "decodes
   pixel exact" and removing the refusal, in the same commit as the gate
   flip, per charter.
4. The `palette_bsize_ctx` rect-strip refusal and `enable-intrabc` are
   still fully unstarted (r4's original "Then" list) -- UV palette turned
   out to be a hard prerequisite of Y, so it should stay first in line
   next round rather than being deferred again.

`deferred: the reorder fix + UV palette mode_info port -- root cause
pinned this round via a decisive range trace (compare-range-not-tell),
implementation not started, next round's first job.`
`deferred: palette_bsize_ctx rect-strip refusal, intrabc -- unstarted,
all budget went to the bisect + root-cause confirmation this round.`
