# lane-rectres — non-skip residual on Rect128/Ab128 pieces

Base: `3b4673fe` (lane-rectres worktree). Disposition: **stays-off (BD
unmeasured — fixtures prerequisite missing; see §4)**. `EC_AV1_RECTRES`
keeps its default (off): OFF is today's behavior bit for bit — the search
never flips a rect/AB piece to non-skip, so the writer's lifted refusal is
never reached. Pins untouched at 8291 / 33227 (no default change, so no pin
run, no lib suite). Never merged, never pushed.

## 1. What shipped (commit `26a9b79e`)

- `write_inter_block_128_rect` (tile.rs): the skip-only refusal is lifted.
  A residual piece codes one TX_64X64 luma unit plus a TX_32X32 chroma pair
  per 64x64 mu chunk (two chunks for a 128x64/64x128 half, one for a 64x64
  square), var-tx depth 0; under `TxMode::Select` a half codes two depth-0
  `txfm_partition` flags (ctx rect at `w.max(h)=128`), a square one (via
  `write_tx_syntax_inter`). A half's chroma unit takes the +3
  `get_txb_ctx` offset; a square's takes none. Luma `txb_skip_ctx`: the
  neighbour magnitude table on a half, fixed 0 on a square (the transform
  IS the whole plane block — `read_inter_plane`'s
  `luma_skip_ctx.unwrap_or(0)`, the 64 root's own writer rule). Tail =
  `record_planes_rect(luma=false)` off the assembled chroma grids, then the
  per-chunk chroma re-stamp on halves only (`mu_chroma` mirror).
- `search_rect_residual` (encode.rs): prices the non-skip outcome against
  the skip arm the search just committed (per-chunk
  `code_from_prediction` trials, `unskip` skip-symbol delta, packed 64-wide
  level grids), commits + flips `block` on a win; wired into
  `search_root_128_rect` and `search_root_128_ab` behind
  `rectres()` (`EC_AV1_RECTRES`, default OFF, test force
  `force_b128_rectres`).
- `cdef_unit_owner` (tile.rs) extends past Whole128: a non-skip Rect128
  half collapses its 2 units onto its origin; an Ab128 half likewise (its
  64x64 squares are 1 unit each). Skip-only pieces never touch the map —
  they code no literal and the decoder's per-piece `cdef_transmitted`
  reset never fires. This is the lane-edge128 bug class, priced for the
  ONE index the syntax carries.
- Census: the native arm's standing line now prints "rect/AB pieces with a
  residual N (search) / M (writer)" and zeroes both counters per clip.
- A defect found and fixed during witnessing (would have been the lane's
  own bug class): the first draft priced the 64x64 square's luma
  all-zero flag off the neighbour table; the decoder fixes it at 0 for a
  whole-block transform, and the desync surfaced as an empty-slot
  reference refusal mid-stream (encode.rs witness fixture), not a pixel
  miss. Fixed before any gate measured anything.

## 2. Witnesses (all `--ignored --exact`, standalone, all PASS)

- `a_rect128_half_with_a_real_residual_decodes_exact_through_both_decoders`
  (smpte pan 1280x768, 8 frames, q=60, HORZ forced + residual forced):
  search won 840, writer coded 840; `coded > 0` guard; whole stream
  sample-exact through `decode_stream` AND ffmpeg, all planes/frames.
  EVIDENCE: ~/.cache/rectres/wit1.log | forced HORZ + rectres, q=60, 8
  frames | "search won 840, writer coded 840", ok, both decoders exact.
  LOADAVG 118.48/98.99/57.92 before, 118.48/98.99/57.92 after (sibling
  lane storm; 3.6s run).
- `an_ab128_piece_with_a_real_residual_decodes_exact_through_both_decoders`
  (testsrc2 1280x768, 12 frames, q=60, HORZ_A forced + residual forced):
  search won 4620, writer coded 5760 (writer counts include the filter
  search's winning re-code); exercises the two-chunk half AND both
  one-chunk squares; byte-exact through both decoders.
  EVIDENCE: ~/.cache/rectres/wit2.log | forced HORZ_A + rectres, q=60, 12
  frames | "search won 4620, writer coded 5760", ok. LOADAVG
  107.04/97.41/58.06 before, 104.55/100.35/63.37 after.
- `a_rect128_residual_half_under_a_per_unit_cdef_list_decodes_exact`
  (testsrc2, 12 frames, q=90, HORZ forced + residual forced): writer coded
  2760 non-skip halves; 62 64x64 CDEF units took another unit's literal
  (owner-map collapse firing on rect pieces); byte-exact through both
  decoders. EVIDENCE: ~/.cache/rectres/wit3.log | --ignored --exact run |
  "writer coded 2760; 64x64 CDEF units taking another unit's literal: 62",
  ok. LOADAVG 7.90/7.85/9.32 before, 9.39/8.16/9.37 after.

## 3. BD measurement — NOT RUN (prerequisite missing)

The control attempt of `bd_rate_film_long_gop` (gated
`systemd-run --user -p MemoryMax=14G --wait
--unit=rectres-longgop-control`) skipped by name: the worktree has no
`fixtures/` — `h264-1080p/2160p...mp4` and
`real-library-manifest.tsv` are absent, and the gate resolves fixtures at
`CARGO_MANIFEST_DIR/../../fixtures` INSIDE this worktree
(`~/.cache/rectres/longgop-control.log`: five "SKIP ... missing" lines,
"SKIP bd_rate_film_long_gop: no clip"). Media files are read-only and the
main checkout must not be touched, so the gate needs the worktree's
fixtures directory populated (copy or symlink of the main checkout's
`fixtures/`) before control and arm (`EC_AV1_RECTRES=1`) runs. Neither BD
table, fire census per clip, nor wall deltas exist yet; the keep rule is
UNMEASURED.

UNBLOCK (verified 2026-09-12): the media fixtures themselves DO exist in
the main checkout's `fixtures/` (e.g.
`/home/tahinli/Documents/Code/Rust/edith_codecs/fixtures/video/av1-1080p-23.976-8bit.mp4`
and `real-library-manifest.tsv`); only the worktree's copy is missing.
From the worktree root: `ln -s ../edith_codecs/fixtures fixtures` (a
worktree-side symlink; the media stays read-only), then re-run the two
gates: control (`cargo test -p ec-av1 --release --lib -- --ignored
--exact --nocapture encode::tests::bd_rate_film_long_gop`) and the same
under `EC_AV1_RECTRES=1`, then `bd_rate_screen_native` both ways, each
under `systemd-run --user -p MemoryMax=14G --wait --unit=rectres-<name>`,
one at a time, LOADAVG before/after.

## 4. Disposition

`stays-off`. Charter branches: mismatch → stopped (none: all witnesses
exact); keep rule met → ships-on (unmeasurable here); inert+exact → stays
off. With BD unrun the only safe disposition is default OFF — which is
bit-exact today's behavior by construction (the knob gates the only path
that flips a piece to non-skip). `fix-now | deferred(<unblock>) |
accepted`: BD control+arm runs and the census report are
`deferred(worktree fixtures/ populated with the real-library manifest)`;
witnesses, writer, search arms, owner map and census line are shipped and
accepted. Pins unmoved at 8291 / 33227. No suite run (default did not
flip).

## 5. Non-goals respected

Main checkout untouched, never pushed/merged, no repo-wide formatters (a
scoped rustfmt reflow of the two edited files was fully reverted by
restoring HEAD bytes and replaying the semantic edits — the committed diff
is semantic-only, 643+/39-), media files read-only, one gate at a time,
logs under ~/.cache/rectres/.
