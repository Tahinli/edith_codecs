# lane-av1-cdf r1 — the tile adapts its CDFs

## What changed
`crates/ec-av1/src/cdf_state.rs` (new) holds the CDF state a key frame's tile
adapts: one owned copy of every table the writer touches, seeded from the
defaults in `crate::cdf`, plus the four coefficient table sets (`TxbSet`) a
transform block is coded with. `sb_coeff_key_frame_tile` now writes every
non-literal symbol through `SymbolEncoder::symbol` against that state instead
of `symbol_fixed` against the static defaults, and `encode_key_frame`'s header
carries `disable_cdf_update: false` (with `disable_frame_end_update_cdf: true`,
since nothing reads what one frame leaves behind).

Tables that a decoder keeps one copy of are one copy here too: a 64x64 luma
transform reads the 32x32 base-range rows, both luma sets read one DC sign
table and one 1024-position end-of-block table, and both chroma sets share
their own DC sign table.

Not adapted: the gathered partition CDF an edge superblock reads, which the
decoder builds for the read and throws away.

## Gates
- `cargo test --release -p ec-av1`: 82 passed, 0 failed, 3 ignored. The
  ffmpeg/dav1d bit-exact roundtrips are the real gate — a decoder that adapts
  its own tables desyncs the moment our order or our sharing differs.
- `cargo clippy --release -p ec-av1 --all-targets`: 0 warnings. `cargo fmt`
  clean.
- New unit tests in `cdf_state`: the two luma sets share the base-range table;
  the sign tables are shared within a plane type and not across; the luma
  end-of-block table is shared and chroma's is not; the state starts at the
  defaults.

## Mutation kills
| Mutation | Killed by |
|---|---|
| DC sign written with `symbol_fixed` (stops adapting) | 4 tests, incl. both ffmpeg roundtrips |
| `Luma64` given the chroma base-range table (breaks sharing) | 3 tests, incl. the 64x64 ffmpeg roundtrip |

## Rate, against the merged baseline
Same encoder, same decisions; adaptation on vs off, BD-rate over the
three-point ladder (`probe_ladder`, `EC_AV1_CLIPS` on his files).

| Picture | BD-rate |
|---|---|
| Troy 1080p AV1 (frame at 600 s) | **-3.52%** |
| OBS screen capture 2026-08-25 (frame at 5 s) | **-5.63%** |
| test card | -11.25% |
| stripes | -37.67% |
| diagonal | -3.54% |

## Dispositions
- The coefficient rate term (`luma_32_coeff_bits`) still prices against the
  **default** tables: the mode search runs before the tile is written, so the
  adapted state the block will really be coded against does not exist yet —
  **accepted**; pricing against a running copy needs the search and the writer
  interleaved, which is its own lane.
- The DC fixture writers (`flat_key_frame_tile`, `dc_key_frame_tile_levels`,
  `split_dc_key_frame_tile`) still code against the defaults; their test frames
  now say `disable_cdf_update: true` — **accepted**, they are fixtures the
  encoder does not use.

## Not in this lane
Block sizes below 32x32, transform types beyond DCT_DCT, inter frames, angle
deltas other than zero, trellis of levels.
