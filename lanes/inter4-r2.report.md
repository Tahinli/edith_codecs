# lane-inter4 r2 — rectangular INTER residual coding

Branch `lane-inter4`, worktree `/home/tahinli/Documents/Code/Rust/edith_codecs-inter4`,
off r1's `59ccb01` (main `146896f`). No rebase this round (main unchanged since).

## Owed from r1: the full suite at 59ccb01

    $HOME/.cache/inter4-suite-r2a.log
    test result: ok. 323 passed; 0 failed; 30 ignored; finished in 458.10s

r1's two self-failures were indeed fixed by `59ccb01`.

EVIDENCE: $HOME/.cache/inter4-suite-r2a.log | ec-av1 --lib under systemd-run at 59ccb01 | 323 passed / 0 failed / 30 ignored

## What this round built (capability present, NOT claimed proven)

Rectangular inter residual coding for the 2:1 shapes whose tables exist —
32x16 / 16x32 (`crates/ec-av1/src/decode.rs`):

- `rect_inter_residual_supported` (decode.rs) — the shape gate. The r1 refusal
  string ("a non-skip rectangular (HORZ/VERT/HORZ_B) strip needs rectangular
  residual coding") is now conditional on it, so `refusal_inventory` is
  unchanged and 64x32/64x16/32x8/8x32/16x8 keep refusing by name.
- `read_block_tx_size_rect` — spec 5.11.17 from the block's own *rect*
  `max_txsize_rect_lookup` entry. `TX_MODE_LARGEST`: no symbol, one rect TU.
  `TX_MODE_SELECT`: one depth-0 `txfm_partition` symbol whose context comes
  from the new `txfm_partition_ctx_rect` (above compares `tx_size_wide`, left
  `tx_size_high`, category from `txsize_sqr_up_map`); `0` leaves one rect TU,
  `1` splits into `sub_tx_size_map`'s two SQUARE sub-transforms from which the
  existing square `read_var_tx_size` continues at depth 1 — so every leaf of a
  split rect block is square and the existing square leaf loop reconstructs it.
- `txfm_partition_update_rect` / the skipped-inter case — a skipped rect inter
  block records its own BLOCK width above and height left (libaom
  `set_txfm_ctxs(.., skip && is_inter)`), where r1 recorded a square `side`
  both ways.
- `read_inter_plane_rect` + `embed_rect_grid` — rect coefficients via
  `read_coeffs_rect` (TxClass-aware scans/contexts), rect inverse via
  `dequant_and_inverse_typed_wh`, added onto the top-left `w x h` of the
  square-strided MC predictor. `txb_skip_ctx` 0 for luma (this unit IS the
  block: `get_txb_ctx`'s `plane_bsize == txsize_to_bsize[tx_size]` branch);
  chroma at `av1_get_max_uv_txsize` (16x8 / 8x16, `TxbSet::ChromaRect16x8`),
  inheriting the luma unit's `tx_type`.
- `cdf_state.rs`: new `TxbSet::LumaRect32x16Inter` — `LumaRect32x16`'s
  coefficient tables (512-coefficient `eob_pt`) plus the inter `tx_type`
  alphabet `default_inter_ext_tx_cdf[3][TX_16X16]` (`EXT_TX_SET_DCT_IDTX`,
  which is what `av1_get_ext_tx_set_type` returns for an inter TU with
  `tx_size_sqr_up == TX_32X32`) — i.e. the existing `INTER_TX_TYPE_SET3_16`.
- Two out-of-bounds fixes found by the new content (both in r1 code, both hit
  before any of this round's residual code runs): `around_mi(at, side)` read
  `side` cells left for a rect strip and indexed past the tile's last mi row
  (now `around_mi_rect(at_mi, write_w, write_h)` for a rect strip); and
  `record_rect_mi`/`record_split_luma_rect_mi` stamped past the frame's last
  16-px column for an edge strip (now bounds-checked).
- `EC_AV1_RECT_DUMP=1` prints the shape of every refused rect strip.
- New counters `rect_inter_tu_hits` / `rect_inter_txsplit_hits` + accessors.

## Gate — RED, kept and `#[ignore]`d with its exact reason

`a_real_aomenc_inter_sequence_with_a_coded_rectangular_residual_decodes_pixel_exact`
(stream.rs): 16 attempts x {2 axis-structured sources, 2 quantisers, 2
`--enable-tx-size-search` arms, 2 motion steps} x {8-bit, 10-bit}, 192x128,
6 frames, `--enable-rect-partitions=1 --enable-ab-partitions=0
--enable-1to4-partitions=0 --min-partition-size=16 --max-partition-size=32`,
overrides last; every decoded frame compared Y/U/V to ffmpeg, refusals counted
never SKIPped, out-of-scope attempts still required pixel-exact.

It never fires, and the reason is NOT the residual code: on real motion content
the decode stops earlier, every attempt, at one of

    an inter partition below 16x16 other than SPLIT (16x8/8x16 rect inter leaves are not coded yet)
    an intra-coded HORZ/VERT strip needs rectangular intra prediction this decoder does not code yet
    an inter partition below 8x8 ...

so no stream ever reached a 32x16/16x32 inter strip. Content sweep this round
(4 source families x cq 22..61): flat/banded sources make the strips `skip`
(r1's case) or trip aomenc's screen-content detector into an intra-strip
refusal; textured/natural ones reach 16-level rect leaves first. `EC_AV1_RECT_DUMP`
on the earlier recipe showed the only rect shapes actually refused were 32x8 /
8x32 (the 32-level 1:4 arms), never 32x16.

EVIDENCE: `cargo test -p ec-av1 --lib coded_rectangular_residual -- --nocapture` | 16 aomenc encodes x 8-bit, decode + ffmpeg Y/U/V compare | 15 named refusals, 0 attempts carrying a rect inter residual, whole-block rect TUs=0 -- gate proved nothing, so it is `#[ignore]`d with that sentence, not weakened

EVIDENCE: `cargo test -p ec-av1 --lib superblock_level_rect_partition -- --nocapture` | r1's skip-strip gate re-run on this round's tree | 4 named refusals, 2 pixel-exact attempts carrying the arm, 64x32 strips=4, 0 mismatches -- the rect skip-context change did not regress it

## Suite

    $HOME/.cache/inter4-suite-r2.log (systemd-run, unit inter4-suite-r2)

## Residue

- **fix-now (r3): 16x8/8x16 inter rect leaves.** They are the real reach
  blocker: real content codes one before any 32-level 2:1 strip. Needs (a) the
  16-level rect inter partition arm, (b) `default_inter_ext_tx_cdf[2][TX_8X8]`
  (12-symbol `DTT9_IDTX_1DDCT` at `tx_size_sqr == TX_8X8`, reachable only
  through a rect transform) as a new CDF + adaptation field, (c) a
  `TxbSet::LumaRect16x8Inter`. The residual machinery this round added then
  covers them unchanged.
- **deferred: the 32x16/16x32 gate** — unblocked by the item above (or by a
  recipe that reaches a 32-level strip first; four source families did not).
- **deferred: 64x32/32x64/64x16/16x64 and 32x8/8x32 rect residual** — 64-point
  and 1:4 rect transforms have no coefficient tables here; still refused by name.
- **deferred: film probe** — not re-run: r1 measured both Troy extracts stopping
  at lane-rectchroma's INTRA refusal ("a coded HORZ/VERT strip whose chroma
  transform has no rect coefficient tables here"), which nothing in this diff
  touches.
- **accepted: a rect block's luma/chroma coefficient grid is embedded in the
  square grid** the downstream neighbour/deblock stamps expect (top-left
  placement). The split-leaf path already hands those stamps an all-zero square
  grid, so this is strictly more information than the shipping behaviour.
- **DO NOT MERGE as a capability claim**: rectangular inter residual coding is
  implemented and compiles, and is exercised by no green gate (class
  refusal-lifted-without-a-gate). The refusal itself is still in place for every
  shape but 32x16/16x32.
