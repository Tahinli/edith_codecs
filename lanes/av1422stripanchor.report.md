# lane-av1-422stripanchor — `decode_rect_split`'s 4-wide-strip chroma ported to
# the split4 floored-pair anchor (`chroma422_pair_plane`), the ported pair
# plane tiled in the (4, 8) OOB units, and the (2, 16)/(2, 8) refusal shapes
# handled — frame 0 of the pinned t422 fixture stays byte-exact at
# PREFILT/POSTDEBLOCK/POSTCDEF/FINAL (first round with the FINAL rung
# measured), all 16 frames decode, and the port is measured inert on the
# fixture (`chroma422_pair_wide: 0`): NO stream reaches it today, so the lift
# is by construction and stays unwitnessed

## Outcome

1. `chroma422_pair_plane` (decode.rs, beside `depth_to_tx_wh`): the split4
   floored-pair anchor ported to `decode_rect_split`'s 1-mi-wide strips. A
   strip whose own chroma block would be 2 px wide at ss (1, 0) — luma
   `(4, 16)`/`(4, 8)`, the footprints `av1_ss_size_lookup[BLOCK_4X16]`/
   `[BLOCK_4X8][1][0]` mark BLOCK_INVALID (verified in the pinned oracle's
   `common_data.c`, not from prose) — codes the PAIR's `(4, bh)` plane:
   WRITE/PREDICTION anchor `((mi_col & !1) * MI) >> ss_x`
   (`av1_setup_dst_planes`' mi-exact floor, the lane-av1-422kf2/
   `sub8_leaf_chroma422` rule), entropy cell at the floored mi column, and
   the odd-column strip is the chroma reference (`is_chroma_reference`'s
   `mi_col & 1` clause, the rule `decode_rect4_16_strip`'s pair16 arm
   implements one partition level up). `None` on every other cell: 4:2:0 and
   4:4:4 can never produce a 2-px-wide chroma block, so their routing is
   unchanged per format cell.
2. The skip and tiled chroma arms take the pair geometry: per-(4, 8) unit
   walks over `(chroma_block_w, chroma_block_h)` at the floored anchor
   `chroma_x` (the blockhalf lane's per-unit skip-push lesson kept),
   `around_mi_422_chroma` per chroma cell, `bigger` forced (the invalid
   plane's `num_pels_log2_lookup[255]` byte reads 0xff — the av1422q +3 rows)
   , CfL source over the PAIR's 8-px luma band, and the chroma `reach` is the
   pair block's (`Reach::of_rect(8, bh, pair_px, py, ..)`), not the odd
   strip's own. Every pre-existing expression reduces to its old value when
   `chroma_422_pair_wide` is false; the lossless raster arm is untouched (its
   domain cannot present a 4-wide strip).
3. The `(2, 16)` refusal (the inventory string "a coded HORZ/VERT strip whose
   chroma transform has no rect coefficient tables here") therefore no longer
   fires for the 4-wide shapes — they are handled. The string STAYS: it
   remains the shape guard for genuinely-untabled shapes, and the
   lane-t900-r33 enumeration test still pins the five callers.
4. GATE: `chroma422_pair_plane_matches_the_ss_size_lookup_cells` — a
   format-cell census over all 22 `BLOCK_SIZES_ALL` footprints: the pair arm
   fires exactly on the 2-px-wide slice of the real `ss_size_lookup[..][1][0]`
   BLOCK_INVALID column ((4, 8), (4, 16); BLOCK_4X4 excluded as the square
   path's own sub-8 rounding rule), the tall/wide BLOCK_INVALID shapes stay
   on `chroma_422_oob` (the two arms partition the column), and 4:2:0/4:4:4
   return `None` at every footprint. Floored-anchor math asserted for odd and
   even mi columns.
5. WITNESS COUNTER: `CHROMA422_PAIR_WIDE_HITS` (+ accessor, +
   `decode_probe` line). Reads 0 on the pinned fixture — the arm is provably
   inert there.
6. MEASURED (fixture `/tmp/t422.obu`, sha256
   `d3fa4beeb1309033e4a03f5ae5c431c8fa8386ba1ada088684710423c7938129`,
   reused by hash; frame-0 cut `/tmp/t422f0.obu` sha256
   `3dd3f768ef553c02316848c2d3184c3765030c0a6fcb95887e4719add9ded3af`;
   ours = `decode_probe --release` under
   `$HOME/.cache/cargo-target-av1422stripanchor`, oracle =
   `$HOME/.cache/aom-oracle/build/aomdec`, padded-u8-vs-cropped-u8 compare):
   - frame 0: **0 diffs on Y, U and V at PREFILT, POSTDEBLOCK, POSTCDEF and
     FINAL** — the FINAL rung is measured this round (the parent lane's
     round bracketed it by POSTCDEF).
   - all 16 frames decode; counters unchanged from the parent lane
     (chroma422_rect 682, chroma422_sub8 122, chroma422_chunk 344) and
     `chroma422_pair_wide: 0`.
7. WHY NO WITNESS STREAM (the acceptance's fallback): the (2, 16) lift is
   by construction and UNWITNESSABLE today — no current stream can route a
   4-wide strip into `decode_rect_split`. The five callers pass only the ten
   rect footprints with `8 <= min` (the enumeration test pins both the caller
   set and the domain); the real 4-wide readers — `decode_rect4_16_strip`
   (16-level 1:4, whose depth walk is inlined precisely because the chroma is
   pair-owned: the pair16 arm already carries the floored anchor + (4, 8)
   tiling), `decode_leaf_rect8` + `decode_inter_sub8_rect2` (8-level
   HORZ/VERT, both per-piece floored arms from lane-av1-422h/blockhalf) and
   the inter `PARTITION_*_4` walk — never call it. Three fresh
   `aomenc --profile=2` probes (testsrc2 cpu-used=1 + tx-size-search,
   mandelbrot cpu-used=0, smptebars screen tune) all decode or diverge
   without touching the arm. A witness requires a future caller that routes
   4-wide strips here (a capability change the enumeration test will force a
   re-derivation for); the counter is wired so that stream proves itself the
   moment it exists.

## Observed, not chased

- The mandelbrot probe (`s2.obu`, frames 1+) crashes in
  `push_mc_rect_tx` ("range end index 32 out of range for slice of length
  0") — frame 0 alone decodes clean. Read as a downstream symptom of the
  known frames-1+ partition-dispatch desync (mi(8,10), Busra4's lane) on
  heavy-motion content: a desynced shape walk reaches the MC push with an
  empty pred buffer. Not attributed to this port; named for the desync lane's
  ledger.

## Decision

- The header refusal STAYS unconditional in `stream.rs`; the
  `EC_AV1_ALLOW_422_PROBE` bypass was re-applied for the measurement runs and
  reverted byte-identical afterwards (0 occurrences;
  `a_non_420_subsampled_sequence_header_is_refused_by_name` green post-revert).
- 4:2:0/4:4:4 byte-identical by ss-keying, checked per format cell by the
  census gate and by measurement on the fixture.
- `read_coeffs_rect`, the chroma table rows and the 4:2:0 group-tail SKIP arm
  untouched.
- cargo check 0 warnings; targeted gates green (enumeration, 422 refusal,
  lossless sample-exact, rect-strip filter-intra, the new census).
