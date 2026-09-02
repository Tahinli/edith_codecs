# lane-golomb r9 -- the straddling-band defect is CDEF, and a stream-data PANIC is now a refusal

## What changed

* `crates/ec-av1/src/decode.rs` `neighbour_filter` -- returns `Result` and refuses
  "an OBMC neighbour whose switchable interp filter was never recorded" instead of handing the
  `[3, 3]` sentinel to `mc::from_switchable_symbol`, which `panic!`s on any value above 2.
  Both call sites (`build_obmc_prediction`'s above and left passes) already sat inside a
  `Result` function and take `?`.
* `crates/ec-av1/src/refusal_inventory.rs` -- the new refusal string pinned.
* `crates/ec-av1/src/decode.rs` `deblock_plane` -- new `EC_AV1_DEBLOCK_TRACE_V` rung: the
  per-edge `x0/y0/len/level` of the vertical pass plus one line per plane with the loop's real
  `tw`/`th`/`stride` vs `true_*`/crop bounds. That rung is what settled the deblock question below.
* `crates/ec-av1/src/stream.rs` -- `a_frame_edge_straddling_band_decodes_pixel_exact`, an
  `#[ignore]`d pinned test that runs `edge32_gate`'s exact recipe over the four straddle arms
  (192x68 and 68x192, 5 frames, 8-bit and 10-bit, cq 35..61), with `EC_GATE_VERBOSE` naming.

## TASK A -- root cause narrowed to CDEF, stage by stage (fix NOT landed)

Stream: `~/.cache/golomb-tmp/s68.obu`, md5 `45722b0fb7f1707a18e0547cc7a59fa2`, produced by
`edge32_gate`'s own aomenc recipe at 68x192 cq35 8-bit, 5 frames.

| stage | ours vs instrumented aomdec, frame 1 |
| --- | --- |
| reconstruction (`EC_AV1_PREFILT_DUMP`) | **0 luma pixels differ** over the cropped 68 columns |
| post-deblock (`EC_AV1_POSTDEBLOCK_DUMP`, `EC_AV1_DEBUG_SKIP_CDEF=1`) | **matches**, including columns 68..71 |
| final output vs ffmpeg | 145 luma differ, mass in columns 64..67 |

So inter prediction is exact and the charter's "the MC reference read runs past the crop"
hypothesis is refuted a second time (r8 refuted it at the libaom source; r9 refutes it by
measurement: our returned `Picture` is already cropped to `frame_width`, so `mc::sample` clamps
at column 67 exactly as `extend_plane` does). The deblock is also exact -- `frame_width` is the
right clip, confirmed against libaom `set_lpf_parameters`' `if ((width <= x) || (height <= y))`
early-out on `plane_ptr->dst.width`, which is the crop width; forcing the mi-rounded bound
instead made frame 0 RED (17 luma px, columns 65..67), so r7's clip stays.

The divergence therefore enters in **CDEF**, at the last 8x8 CDEF block column (mi column 17,
pixels 64..71) -- the one block that straddles the crop edge. Frame 0 survives it and frame 1
does not, which is why every earlier round read the shape as "inter".

Two instrument facts a successor needs:
* aomdec's `EC_AV1_POSTDEBLOCK_DUMP` is **72 columns wide** (mi-rounded) while its
  `EC_AV1_PREFILT_DUMP` is cropped to 68 -- the post-deblock rung is the only one that exposes
  the straddling band on the reference side.
* our own `EC_AV1_POSTDEBLOCK_DUMP` is emitted from a site that, on this path, runs **after**
  `apply_cdef`: `od.f0` and `odnc.f0` (same run, `EC_AV1_DEBUG_SKIP_CDEF=1`) differ in columns
  68..71 although `EC_AV1_WATCH`-instrumented `filter_edge` performs no write there. Always pair
  that dump with `EC_AV1_DEBUG_SKIP_CDEF=1`, or the CDEF defect reads as a deblock defect --
  which cost r9 six rounds of the budget.

EVIDENCE: ~/.cache/golomb-tmp/{ours,theirs,od,odnc,td}.f* | aomenc 68x192 cq35 5f -> decode_probe with EC_AV1_PREFILT_DUMP / EC_AV1_POSTDEBLOCK_DUMP(+EC_AV1_DEBUG_SKIP_CDEF) vs the same rungs on the oracle aomdec | prefilt f1 luma diffs 0/13056, post-deblock (cdef off) f1 luma diffs 0, final output f1 145 differ first row 0 col 64

## TASK B -- the panic is gone

Before: `192x68 cq40` aborted the process in `mc.rs:203`. After:

```
$ decode_probe s40.obu
REFUSED: unsupported: AV1 tile (an OBMC neighbour whose switchable interp filter was never recorded)
```

EVIDENCE: ~/.cache/golomb-tmp/s40.obu (md5 26d42c5f04e4d0f7f4075a4550e999d5) | aomenc 192x68 cq40 5f -> cargo run --example decode_probe | process abort in mc.rs:203 -> `REFUSED: unsupported: AV1 tile (an OBMC neighbour whose switchable interp filter was never recorded)`

Sweep of the other aborts reachable from a desynced stream (`decode.rs` + `mc.rs`), each checked
against the value's own domain:

* `mc.rs:203` `from_switchable_symbol` -- FIXED at its only reachable caller. The other caller
  (`resolve_interp_filter`) reads a 3-symbol CDF (`switchable_interp: [[u16; 4]; 16]`,
  `nsyms = cdf.len() - 1`), so it cannot produce a 4th symbol.
* `mc.rs:220` `from_header(Switchable)` -- header enum, not a per-block stream value; callers
  resolve `Switchable` before calling. Left.
* `decode.rs:3259` `default_intra_tx_type` -- the mode comes from a 13-symbol CDF and all 13
  values have a row. Bounded, left.
* `decode.rs:13936`/`14114` `partition_w8` and `decode.rs:15352` `read_single_ref` -- values from
  4-symbol / bounded-return helpers. Bounded, left.
* `decode.rs:11673/11692/11739` -- guarded by `rect_inter_residual_supported`, which is itself a
  named refusal. Left.
* `decode.rs:11324`, `12091`, `12110`, `12147`, `15660`, `2745` -- block-size / tx-size domain
  invariants, not stream values. Left.
* `decode.rs:2498` `ref_hits` -- a test-side counter accessor, never on the decode path. Left.
* the non-test `.unwrap()`/`.expect()` in `decode.rs` (5280, 5324, 6085, 6152, 7352/7353,
  7819/7820, 10535, 10591, 10735, 10944) are each dominated by a push/refusal a few lines above.
  Left, listed here so the next sweep does not re-derive it.

## TASK C -- arms

`a_frame_edge_straddling_band_decodes_pixel_exact` carries all four straddle arms and is
`#[ignore]`d, which is the only honest shape while the CDEF defect is open: an arm that is RED
cannot join the green gate, and the r8 handoff's "measured, arm not added" comment is now a
runnable test instead of prose.

fix-now residue, NOT done this round:
* deferred(the CDEF straddle fix) -- the `--enable-restoration=1` arm at 192x68 that would gate
  r8's cropped-plane loop-restoration fix. `edge32_gate`'s arm tuple has no per-arm flag slot and
  every straddle recipe is RED regardless, so the LR arm would be vacuous today; it unblocks the
  moment the CDEF fix lands. This is the same `refusal-lifted-without-a-gate` shape r8 flagged.

## Suite

`cargo test -p ec-av1 --lib` -> see `$HOME/.cache/golomb-suite-r9.log` (totals in the round report line).
