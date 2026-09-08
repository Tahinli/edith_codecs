# lane-last2 — a second past reference (LAST2) on the pyramid

Worktree `edith_codecs-last2`, branch `lane-last2` off main e6df5196.
Charter: `lanes/refs.report.md` §6. Every gate arm is the prebuilt release
lib-test binary running `bd_rate_screen_native` (12 pictures, `gop 12`, four
quantizers, native `gate_crop` window), one film per arm
(`EC_AV1_NATIVE_FILM` / `EC_AV1_NATIVE_FILM4K` / `EC_AV1_NATIVE_SCREEN`); every
row is read off that log's own header line.

## 0. The charter's ARF step is a stale premise (read this first)

The charter orders "ARF level first". At the ARF level there is nothing to
build: a group's top ARF ALREADY names two distinct past anchors --
`last_slot` = `ANCHOR_SLOTS[anchor]` (the previous group's top ARF, lag
`mini_gop`) and, since lane-arfcen's `arf_altref` shipped ON,
`altref_slot` = `ANCHOR_SLOTS[1 - anchor]`, which still holds the top ARF from
TWO groups back (lag 2 * `mini_gop`) when the frame is coded. Those are
exactly the census's (4, 8) ARF lags, and that reference is offered to the
32x32 search through the `extra` path WITH its own `NEWMV` search
(`EXTRA_REF_NEW_MV` = true, margin `EXTRA_NEW_SKIP_MARGIN` = 0.35). So the
census's ARF row (film A 10.31%, film B 8.38% of prediction SAD) is a lever
already banked by `arf_altref`, not an open one; naming the same slot again as
`LAST2_FRAME` would offer the same picture twice.

The open half of the census is the LEAF chain (film A 6.99%, film B 7.25%):
a leaf has exactly one past picture in the DPB (`LEAF_SLOT`, or the mid /
quarter hidden frame behind it) plus the key as `GOLDEN`. That is what this
lane builds.

## 1. The slot map

| slot | holds | written by | read as |
|---|---|---|---|
| 0, 7 (`LEAF_SLOTS`) | the last two shown leaves | every leaf, alternating | `LAST_FRAME` and `LAST2_FRAME` of the next leaf |
| 1 (`GOLDEN_SLOT`) | the key frame | the key | `GOLDEN_FRAME` everywhere |
| 2 (`MID_SLOT`) | the mid ARF | the mid ARF | `LAST`/`ALTREF` of the leaves and quarters around it |
| 3, 4 (`ANCHOR_SLOTS`) | this group's anchor and the next | the key, each top ARF | anchor = `LAST`, next = `ALTREF` |
| 5, 6 (`QUARTER_SLOTS`) | the two quarter ARFs (off by default) | the quarter ARFs | `LAST`/`ALTREF` of the leaves beside them |

Leaf `k` refreshes `LEAF_SLOTS[k % 2]`, i.e. the slot leaf `k - 2` wrote --
read before write. `Av1Encoder::leaf_hist` carries the last two leaf slots
ACROSS mini-GOP boundaries (the first leaf of a group reads the anchor as
`LAST`, so its `LAST2` is the previous group's last leaf) and is cleared by
every key frame (which writes all eight slots with one picture `GOLDEN`
already names). A leaf whose `LAST` is a hidden frame reads the previous leaf
as `LAST2`. The hidden levels pass `None`: see §0.

