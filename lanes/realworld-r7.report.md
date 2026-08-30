# lane-realworld r7 report

VERDICT: Job 1 (merge main into lane-realworld) DONE, gated green, committed
(f2e516f). Job 2 (high-bit-depth typing plan) DONE as scope-only, written
below; no code changed for it, nothing half-wired.

## Job 1 -- merge (f2e516f)

`git merge main` (main at 92d8beb) hit the nine conflicts the charter named:
eight hunks in `crates/ec-av1/src/stream.rs`, one in
`decode_inter_frame_tile_with_cdfs` in `crates/ec-av1/src/decode.rs`.

- `stream.rs` line ~173-204: kept HEAD's comment (delta_q/delta_lf already
  read/applied, r5) plus main's new bit-depth refusal (`"a stream whose bit
  depth is not 8..."`, kept verbatim per the charter); dropped main's
  now-stale `"a frame with delta_q_present or delta_lf_present set..."`
  refusal since that capability is this branch's own finished work.
- `stream.rs` lines ~6545-6893: these seven remaining conflict markers were
  git's line-based diff conflating two *different* new test functions HEAD
  and main each appended at the same spot (HEAD's
  `a_real_aomenc_stream_with_delta_q_and_delta_lf_decodes_pixel_exact`, main's
  `a_real_aomenc_stream_with_two_tile_columns_and_an_inter_frame_decodes_pixel_exact`)
  -- not a real overlap. Resolved by pulling each side's complete function
  from `git show HEAD:.../stream.rs` / `git show main:.../stream.rs` and
  placing both, intact and un-interleaved, one after the other.
- `decode.rs` (`decode_inter_frame_tile_with_cdfs`): main reworked the
  function from a single tile-wide decoder into the same per-tile loop
  `decode_key_frame_tile_with_cdfs` already had (fresh `cdfs`/`dec` per
  tile, `base_cdfs`/`result_cdfs` split). HEAD's delta_q/delta_lf state
  setup (`DELTA_Q_PRESENT`/`DELTA_Q_RES`/`DELTA_LF_PRESENT`/`DELTA_LF_RES`/
  `DELTA_LF_MULTI`, all frame-level and `q_present`/`res` etc from
  `DeltaParams`) now lives once before the loop, same as the intra tile
  function; `CURRENT_Q_IDX`/`CURRENT_DELTA_LF` (both genuinely per-tile,
  spec `decode_tile` resets `CurrentQIndex` at the top of every tile) moved
  inside the loop, mirroring `decode_key_frame_tile_with_cdfs`'s own
  `CURRENT_Q_IDX.with(|c| c.set(i32::from(base_q_idx)))` /
  `CURRENT_DELTA_LF.with(|c| c.set([0; 4]))` pair placed right where that
  function creates its own per-tile `cdfs`/`dec`.
- One knock-on compile error the merge exposed, not a conflict: main's
  `a_real_aomenc_stream_with_four_tile_columns_decodes_pixel_exact` test
  (auto-merged cleanly, no conflict marker) calls
  `decode_key_frame_tile_with_cdfs` directly and was missing the trailing
  `delta: DeltaParams` argument this branch's signature already carries
  (r4). Added `header.delta` (the struct is `Copy`, same pattern every
  other call site in the file already uses) to fix the E0061.

`export CARGO_TARGET_DIR=$HOME/.cache/cargo-target-realworld && EC_AV1_REQUIRE_AOMENC=1 nice -n 19 cargo test -p ec-av1 --lib -j4`:
**246 passed, 0 failed, 17 ignored** (was 243 pre-merge on this branch, 239
pre-merge on main; the merge is additive, no regressions).

No refusal strings added/removed/reworded beyond what main already carried
in (the bit-depth refusal, kept verbatim; the three reworded partition
refusals, untouched by this merge).

## Job 2 -- high bit depth: typing plan, not implemented

Both real files in this box's library
(`~/Downloads/The.Hunger.Games...2160p.AV1.HDR10...mkv`,
`~/Videos/Films/Troy...1080P.AV1...mkv`) are `yuv420p10le` and now stop at
the bit-depth refusal landed by this merge. Turns ran out before probing
them live with `decode_probe`/`ffmpeg -f obu` this round -- next lane's
first move, not scoped further here.

### Why `Vec<u8>` cannot just get a runtime scale

`transform.rs` already takes `bit_depth: u8` and uses it for the inverse
transform's own intermediate clamp widths (`row_clamp = bit_depth + 8`,
`col_clamp = max(bit_depth + 6, 16)`, `transform.rs:829-830`) -- that part
is bit-depth-aware today. Every dequant call site in `decode.rs` still
hardcodes the literal `8` as that parameter (6 call sites:
`dequant_and_inverse_typed`/`dequant_and_inverse_typed_wh`, e.g.
`decode.rs:2986`, `:3316` intra rect luma/chroma x2 each plus the inter
paths at `:5397`, `:5858`). Threading the real `bit_depth` through those 6
is narrow (same shape as `base_q_idx`'s existing pass-through).

The actual width problem is downstream of the transform: **every
reconstruction write clamps to `[0, 255]` and stores `as u8`.** This is
not 2 sites, it is a whole-crate convention:
`decode.rs`'s `PlaneBuf::reconstruct`/`reconstruct_rect` (`:3153`,
`:3226`), CDEF pixel writes (`:4181`), deblocking's op/oq stores (`:4634`
-`:4710`, ~14 sites across the 4/6/8/13/14-tap filters), plus every
sample buffer write in `mc.rs` (inter prediction, `:406`/`:524`/`:587`)
and `intra.rs` (intra prediction proper, `:307`/`:434`/`:568`, and filter
intra `:762`). All of these assume an 8-bit sample IS a `u8`, both as the
storage type and as the clamp bound.

### The type to widen

`Picture` (`encode.rs:86`, `y`/`u`/`v: Vec<u8>`) and `PlaneBuf`
(`decode.rs:2980`, `data: Vec<u8>`) are the two sample-storage structs.
Widening both to `Vec<u16>` (AV1 caps at 12-bit, so `u16` covers every
legal `bit_depth`) is the natural target -- but doing that unconditionally
regresses every existing 8-bit fixture's memory/perf for a capability the
crate doesn't have yet. The narrower move: keep `Vec<u8>` as the on-disk
`Picture` representation for the 8-bit case (all current fixtures, all
current gates, `Picture::grey`'s `128u8` default) and give `PlaneBuf`
(the internal, per-decode scratch buffer, never a public API surface) a
generic sample type — either a `u16`-always internal buffer (simplest;
8-bit content just never uses the top byte, same choice `dav1d`/libaom
make internally) or a `PlaneBuf<S>` generic over `u8`/`u16` picked once at
`decode_key_frame_tile_with_cdfs`'s entry from the sequence header's
`bit_depth`. Given this crate has no perf-critical caller depending on
`PlaneBuf` staying byte-sized (it is decode-internal, dropped at the end
of each frame), **`u16`-always is the lower-risk pick**: one type, no
monomorphization/dyn-dispatch fork through the ~30 clamp-and-store call
sites, and the existing 8-bit path just runs with a wider-than-needed
sample width, which every reference decoder already does internally.

### Concrete surface, sized

1. `PlaneBuf.data: Vec<u8>` -> `Vec<u16>`; every
   `.clamp(0, 255) as u8` write becomes `.clamp(0, max_val) as u16` where
   `max_val = (1i32 << bit_depth) - 1` (a new thread-local, same pattern
   as `CDEF_BITS`/`ENABLE_EDGE_FILTER`, set once per tile-function entry
   from the sequence header's `bit_depth`). ~30 call sites across
   `decode.rs` (reconstruct/reconstruct_rect/CDEF/deblock), `mc.rs`
   (inter prediction), `intra.rs` (intra + filter-intra prediction) --
   every one is already a `.clamp(0, 255) as u8` grep hit, so the sweep is
   mechanical once the constant is threaded, not a design problem.
2. `prediction: Vec<u8>` scratch buffers inside `reconstruct`/
   `reconstruct_rect` (`decode.rs:3121`/`:3182`) and the `dst`/`out`
   parameters `mc::predict`/`intra::predict`/`predict_filter_intra`
   already take by `&mut [u8]` -- these become `&mut [u16]`, a pure
   signature widen, no logic change (the intermediate math is already
   `i32`/wider before the final clamp-and-store).
3. `transform.rs`'s 6 `bit_depth: 8`-hardcoded call sites in `decode.rs`
   become the real `bit_depth: u8` from the sequence header's
   `color_config.bit_depth`, threaded the same way `base_q_idx` already
   is (a plain pass-through parameter, no new state needed — confirmed
   the value is already carried on `SequenceHeader.color_config` and
   reachable at every one of these 6 call sites' enclosing function).
4. `Picture` (the public, post-decode return type) gains a `bit_depth: u8`
   field and its `y`/`u`/`v` become `Vec<u16>` too (matching what
   `ffmpeg_decode_sequence`'s gate helper will need to compare against,
   below) -- OR stays `Vec<u8>` for 8-bit and the crate adds a parallel
   `Vec<u16>` accessor/variant for >8-bit. Given no downstream consumer of
   `Picture` in this crate needs 8-bit specifically (it's an internal test
   type per `encode.rs`'s own doc comment "one 8-bit planar picture"),
   the simpler move is widening `Picture` itself to `Vec<u16>` too and
   dropping the "8-bit" from its doc comment -- `Picture::grey`'s `128`
   default is bit-depth-agnostic already (mid-grey is `1 << (bit_depth-1)`
   either way, one more thread-local read).
5. The refusal (`stream.rs`, this merge's `"a stream whose bit depth is
   not 8..."`) is deleted once (1) through (4) land and a gate proves them,
   the same sequencing every other capability in this repo's ledger
   followed (interintra, masked compound, wedge, warp).

### What the gate needs -- `ffmpeg_decode_sequence` also assumes 8-bit

`stream.rs:738`'s test helper hardcodes `-pix_fmt yuv420p` on ffmpeg's
raw output, which is 1 byte/sample regardless of source depth (ffmpeg
would silently truncate a 10-bit decode to 8-bit there too, the exact
silent-wrongness class the new refusal exists to avoid). A 10-bit gate
needs a second helper (or a `bit_depth` parameter on this one) that asks
ffmpeg for `-pix_fmt yuv420p10le` and parses the raw output as
little-endian `u16` pairs, 2 bytes/sample -- mechanical, not a design
question, but it is new code, not reuse of the existing 8-bit gate
helper as-is.

### Sizing

This is at minimum its own lane, likely 2: one for the mechanical
`u8`->`u16` PlaneBuf/prediction-buffer widening + real `bit_depth`
threading through the 6 transform call sites (large diff, low risk --
every site is already a grep-identifiable `.clamp(0,255) as u8` pattern),
gated first against synthetic 10-bit `aomenc` fixtures; a second for
whatever the two real films' streams refuse on *next* (the charter's own
prediction: "expect a new refusal to appear behind the bit-depth one").
Not attempted this round -- charter scoped this as "the shape, not the
whole feature," and every comparable feature in this repo's ledger
(warp, OBMC, interintra, masked compound, wedge) took multiple dedicated
lane-rounds; landing even the mechanical half in whatever budget remained
after Job 1 risked leaving a half-wired widening uncommitted at the turn
cap, which the charter explicitly forbids ("land nothing half-wired").

deferred: probing the two real films with `decode_probe`/`ffmpeg -f obu`
to name the next refusal — no turns left this round — unblocked by
nothing (any future lane can run it immediately); the typing plan above
does not depend on that probe's output, it depends only on the crate's
existing `u8`-everywhere convention, which is repo-visible without the
films.

## Merge note for future lanes

`gate_coverage.rs` / `refusal_inventory.rs` needed no manual entry beyond
what the merge itself carried in (both auto-merged clean, no new gate or
refusal added this round). Sibling worktrees (`-lr`, `-palette`,
`-sbpart`, `-superres`, `-tiles`) were not touched. Nothing pushed.
`lane-realworld` was not merged into `main`.
