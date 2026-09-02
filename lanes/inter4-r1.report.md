# lane-inter4 r1 — inter-frame rect / 1:4 partition arms

Branch `lane-inter4`, worktree `/home/tahinli/Documents/Code/Rust/edith_codecs-inter4`,
off main `146896f` (charter said 8262b99; main had moved two merges further —
the worktree was already at 146896f and was NOT rebased backwards).

## Premise re-measure (charter's premise was stale)

`cargo run --release --example decode_probe` on both Troy extracts, on THIS tree
before any edit of mine, stops at

    unsupported: AV1 tile (a coded HORZ/VERT strip whose chroma transform has no
    rect coefficient tables here)

i.e. an INTRA-frame refusal owned by lane-rectchroma, not "an INTER 32x32 partition
type ... (value=9)". The charter's `value=9` observation was made on lane-rectchroma
587d388, a tree where that intra refusal is already lifted. So Troy's *next* blocker
after rectchroma merges is plausibly the inter 1:4 arm, but this lane cannot observe it.

## The real ceiling this round found

`decode_inter_block` refuses every NON-SKIP rectangular inter block:

    decode.rs:12938  "a non-skip rectangular (HORZ/VERT/HORZ_B) strip needs
                      rectangular residual coding"

Rectangular inter residual coding does not exist. Empirically (30 aomenc recipes swept,
`$HOME/.cache/inter4-probe`), every real stream that reaches an inter rect strip finds it
non-skip and stops there — including every 10-bit attempt of this lane's own gate.
Porting the partition ARMS is therefore necessary but not sufficient: the arms are
reachable, but only an all-skip strip completes.

## Changed

- `crates/ec-av1/src/decode.rs`
  - `decode_inter_block`'s `at` is now in **MI (4 px) units**, not 16-px cells — a 32-level
    1:4 strip sits at an 8-px offset the old unit could not name. All 20 call sites wrapped
    in the new `sub16_to_mi` helper; the body routes through `around_mi`,
    `record_rect_mi` (new), `record_split_luma_rect_mi` (new), `record_inter_rect_mi`,
    `record_compound_ctx_rect_mi`. `record_rect`/`record_split_luma_rect` now delegate to the
    new mi variants, so there is one implementation each.
  - `motion_mode`/`obmc` CDF row selection: `bsize_idx` is a full `(write_w, write_h)` match
    over every shape the decoder can now reach; an unknown shape refuses by name instead of
    reading the wrong-width alphabet (class wrong-alphabet-same-value).
  - `interintra` is no longer read on a 1:4 strip: `is_interintra_allowed_bsize` is
    `BLOCK_8X8..=BLOCK_32X32` in libaom's ENUM ORDER, which excludes BLOCK_8X32 (18) and
    BLOCK_32X8 (19) even though both dimensions are in range (class
    equal-range-means-unread / symbol the reference never wrote).
  - `maybe_read_delta_q`/`_lf`'s `is_whole_sb` is now `bsize == sbSize` (`side == 64 &&
    write_w == 64 && write_h == 64`), not `side == 64` — a 64x32 strip's `skip` must not
    suppress the read.
  - inter tile, superblock level: `PARTITION_HORZ`/`VERT`/`HORZ_4`/`VERT_4` arms
    (two 64x32/32x64 strips, four 64x16/16x64 ones) with libaom's `i > 0` frame-edge break.
    AB at superblock level still refuses, by a new named string.
  - inter tile, 32x32 level: `PARTITION_HORZ_4`/`VERT_4` arms (four 32x8 / 8x32 strips,
    quarter step 2 mi).
  - new counters + accessors: `rect4_32_{horz,vert}_inter_hits`,
    `inter_sb_{horz,vert,horz4,vert4}_hits`.
  - a local `inter_piece!` macro carries the shared 50-argument call instead of six more copies.
- `crates/ec-av1/src/cdf.rs`, `cdf_state.rs` — `OBMC`/`MOTION_MODE` widened 6 -> 12 rows with
  libaom's BLOCK_32X64/64X32/8X32/32X8/16X64/64X16 rows (`default_{obmc,motion_mode}_cdf`,
  `entropymode.c:549,562` of the oracle source).
- `crates/ec-av1/src/refusal_inventory.rs` — the SB-level string now names AB only; new
  "a motion_mode symbol for a block shape this decoder has no CDF row for".
- `crates/ec-av1/src/stream.rs` — `inter_rect_counters()` + the new gate.
- `crates/ec-av1/examples/decode_probe.rs` — prints the six inter rect counters.

## Gate

`a_real_aomenc_inter_sequence_with_a_superblock_level_rect_partition_decodes_pixel_exact`
(stream.rs). 16 attempts x {2 orientations, 4 quantisers, 2 motion steps}, 192x128, 6 frames,
`--min-partition-size=32 --max-partition-size=64 --enable-rect-partitions=1
--enable-1to4-partitions=1 --lag-in-frames=0 --cpu-used=0` (overrides last, aomenc keeps the
last occurrence). Noiseless split-motion source (one half translating, the other static) is
what makes aomenc's RD pick a superblock HORZ whose strips are `skip`. Every decoded frame is
compared Y/U/V to ffmpeg; refusals are counted, never SKIPped; a decoded attempt that carried
no rect strip is still required to be pixel-exact.

    EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib superblock_level_rect_partition -- --nocapture

EVIDENCE: $HOME/.cache/inter4-suite.log + `cargo test -p ec-av1 --lib superblock_level_rect_partition` | 16 aomenc encodes, decode + ffmpeg Y/U/V pixel compare over 6 frames each | 8-bit: 4 named refusals, 2 pixel-exact attempts carrying the arm, 64x32 strips=4, 32x64 strips=0, 10 out-of-scope (0 mismatched); gate ok. Full suite at commit 754a7dd: 321 passed / 2 failed / 30 ignored; both failures were stale pins of MINE (the SB refusal string and a line-continuation in a new refusal literal) and are fixed in the follow-up commit -- the suite has NOT been re-run end to end since.

## Open residue (nothing here is claimed done)

- **fix-now (r2): rectangular inter residual coding.** Until it exists no gate can cover a
  non-skip inter rect strip, and that is 100% of what real content produces. This blocks:
  the 32-level 1:4 inter arms (0 hits in every recipe swept), the SB-level VERT arm (fires,
  then its second strip is non-skip), the SB-level 1:4 arms, and the whole 10-bit side.
- **deferred: the 10-bit twin of the new gate** — every 10-bit attempt found the strip
  non-skip. Unblocked by the item above. Written as an 8-bit-only loop with the reason in
  the source, not a weakened assert.
- **deferred: SB-level AB inter partitions** — refused by name, no gate. Unblocked by the
  same residue (their pieces are rect too).
- **accepted: `size_group` stays 3 for rect pieces** — inert (it only feeds the intra
  `y_mode` branch, which refuses non-square) and it matches the existing 32-level HORZ arm.
- **DO NOT MERGE the two new arms as a capability claim**: the SB-level HORZ arm is gated,
  the SB-level VERT/1:4 and the 32-level 1:4 arms are NOT (class refusal-lifted-without-a-gate).
