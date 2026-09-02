# lane-fistrip r1 — filter intra on a rect strip

Branch `lane-fistrip` off main `df5d630`. Worktree
`/home/tahinli/Documents/Code/Rust/edith_codecs-fistrip`,
`CARGO_TARGET_DIR=$HOME/.cache/cargo-target-fistrip`.

## Charter premise: stale, corrected by measurement

The charter said `predict_filter_intra` is square-only. It is not: lane-rectsplit r1
already gave it `bw`x`bh` (`crates/ec-av1/src/intra.rs:773`), and 32x16/16x32 strips
already predict filter intra with a real-aomenc gate
(`a_real_aomenc_stream_with_filter_intra_on_a_horz_vert_strip_decodes_pixel_exact`).

The real defect one level down was a **symbol-consumption gap**, not a prediction gap:
`filter_intra_size_class_rect` (`decode.rs:3491`) returned `None` for 8x16/16x8, so
`read_intra_mode_rect` never read the `use_filter_intra` flag that
`av1_filter_intra_allowed_bsize` (`wide <= 32 && high <= 32`, oracle
`av1/common/reconintra.h`) makes aomenc write on every DC_PRED 8x16/16x8 leaf. The
`decode_leaf_rect` refusal "filter intra on a HORZ/VERT strip (this decoder predicts
square-only)" (old `decode.rs:4661`) was DEAD CODE behind that `None` — it never fired;
the tile desynced silently instead.

Film stops measured on main `df5d630` (release `decode_probe`), BEFORE any change:
- `hghead.obu` (Hunger Games 0.4 s head): `a partition below 8x8 (this decoder codes no
  leaf smaller than 8x8)` — earlier than filter intra, as the charter suspected, so the
  film is not this lane's gate.
- `hg5.obu`: `a superblock-level HORZ/VERT strip with a split transform (per-unit rect
  prediction is not ported)`.

## Changed

- `crates/ec-av1/src/cdf.rs:424` — `FILTER_INTRA` grows to 8 rows: `[6]`=8x16 (12551),
  `[7]`=16x8 (9394), `default_filter_intra_cdfs` index 4/5 (oracle
  `av1/common/entropymode.c:821`).
- `crates/ec-av1/src/cdf_state.rs:462` — the field's type/doc follow.
- `crates/ec-av1/src/decode.rs:3491` — `filter_intra_size_class_rect` maps
  `(8,16)`/`(16,8)` to those rows, so the symbol is READ where aomenc writes it.
- `crates/ec-av1/src/decode.rs` (`decode_leaf_rect`) — the dead square-only refusal
  replaced by the counters; the strip predicts through the `reconstruct_rect` call that
  was already there.
- `crates/ec-av1/src/decode.rs` — new `FILTER_INTRA_RECT_SUB16_HITS` /
  `filter_intra_rect_sub16_hits()`: a gate asserting the wider `filter_intra_rect_hits`
  would be satisfied by a 32x16 strip and prove nothing about the new shapes.
- `crates/ec-av1/src/refusal_inventory.rs` — the square-only line removed.
- `crates/ec-av1/src/decode.rs` tests — `filter_intra_classes_carry_their_own_libaom_default_row`
  enumerates every shape inside `av1_filter_intra_allowed_bsize` against
  `default_filter_intra_cdfs` row by row and pins the shapes still without a class
  (4x8/8x4/4x16/16x4/8x32/32x8 — their partition levels refuse first).
- `crates/ec-av1/src/stream.rs` — `a_pinned_aomenc_16x8_strip_reads_its_use_filter_intra_flag`
  (green) plus the pixel gate
  `a_real_aomenc_stream_with_filter_intra_on_a_sub16_horz_vert_strip_decodes_pixel_exact`
  (`#[ignore]`d with its measurement, see below).
- `crates/ec-av1/fixtures/filter_intra_8x16_strip_seed49.obu` — the pinned stream
  (md5 `ac3a64773e5454ee9b5d8650507063cd`, hashed twice from the same recipe; `.gitignore`
  ignores `fixtures`, so it is committed with `git add -f`).

## Gate status — honest

The charter's gate (`..._on_a_rect_strip_decodes_pixel_exact`) is written and **cannot be
green on this tree**, so it ships `#[ignore]`d carrying its measurement rather than
weakened: over **200 aomenc streams** (seeds 42..241, cq 25, `--min-partition-size=8
--max-partition-size=32 --enable-filter-intra=1`) aomenc put filter intra on an 8x16/16x8
strip exactly **twice**, and **both** strips were `skip=0`, which
`decode_leaf_rect` still refuses ("a coded (non-skip) HORZ/VERT rect strip below 16x16").
Sweeps that found nothing better: cq {15,20,25,30,34,40,45,55}; content
{gradients, testsrc2, smptebars-style, geq noise}; intra-tool subsets
{none, smooth+paeth off, smooth+paeth+dir+angle off} — the last kills filter intra
entirely. Flag-arrival note (coordinator correction, aomenc keeps the LAST repeated
`--enable-*`): this gate spells every flag once and appends the `EC_FISTRIP_OFF`
diagnosis overrides AFTER the base recipe, and arrival of `--enable-filter-intra=1` is
proven decoder-side by the counters, not by the command line.

10-bit arm (`a_real_aomenc_10bit_filter_intra_on_a_sub16_strip_decodes_pixel_exact`,
`ten_bit_tool_gate`, seeds 42..61, same recipe): **16/20 attempts decode pixel-exact at
10 bits** with the new CDF rows in place (no regression), and the same seed 49 refuses at
the coded-strip ceiling -- `filter_intra_rect_sub16_hits` never reaches a compare, so this
arm is `#[ignore]`d with its measurement too.

What IS proven, green, and pinned: the symbol read. On the pinned stream aomdec's own
`EC_TRACE_MODE` shows four `BLOCK_16X8` (`bsize=5`) DC_PRED leaves; our decoder now reads
`use_filter_intra` on them, fires the counter once, and stops at the coded-strip ceiling
ON that block. Before the fix the same stream refused with a DIFFERENT string — "a
HORZ/VERT intra strip below 16x16 with a split transform" — a `tx_depth` read out of an
already-desynced stream, which is the desync this round removes.

EVIDENCE: `crates/ec-av1/fixtures/filter_intra_8x16_strip_seed49.obu` | pre-fix release
`decode_probe` on main df5d630 vs post-fix `cargo test -p ec-av1 --lib
a_pinned_aomenc_16x8_strip_reads_its_use_filter_intra_flag` | refusal string moves
"split transform" -> "coded (non-skip) rect strip below 16x16",
`filter_intra_rect_sub16_hits` delta 0 -> 1
EVIDENCE: oracle `EC_TRACE_MODE=1 aomdec` on that fixture | grep `bsize=` | 4x BLOCK_16X8
DC_PRED leaves (`skip=0`), 6x 16x16, 4x 32x16-class, 20x 32x32

## Residue

- deferred: pixel-exact gate for filter intra on 8x16/16x8 — blocked on the "coded
  (non-skip) HORZ/VERT rect strip below 16x16" refusal (needs TX_16X8/TX_8X16 luma plus
  8x4/4x8 chroma coefficient tables) — un-ignore the gate the round that lands.
- deferred: the 10-bit pixel arm rides the same ceiling as the 8-bit one.
- deferred: 8x32/32x8 (HORZ_4/VERT_4) and 4x8/8x4 (sub-8x8) filter-intra classes — their
  partition levels refuse before the mode read; the domain test pins them as absent.
- accepted: the capability claim "filter intra on a superblock-level HORZ/VERT strip
  (never expected -- av1_filter_intra_allowed_bsize caps at 32x32)" stays; 64x32/32x64
  are past the <=32 bound on one axis, verified in the same domain test.

## Film probes after the fix (release `decode_probe`, this branch)

- `hghead.obu`: `a partition below 8x8 (this decoder codes no leaf smaller than 8x8)` (unchanged)
- `hg5.obu`: `a superblock-level HORZ/VERT strip with a split transform (per-unit rect
  prediction is not ported)` (unchanged)

Both films stop before any 8x16/16x8 filter-intra strip, so this lane moves neither
frontier; it removes a silent desync that would have bitten once those two lift.

## Suite totals

`EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib` (log `$HOME/.cache/fistrip-suite.log`,
826 s): **300 passed, 1 failed, 29 ignored**.

The one failure is PRE-EXISTING on main `df5d630` and not this lane's: 
`decode::tests::nz_map_ctx_offset_tables_match_the_rect_rule` — "32x64 nz_map offset at
display (row 0, col 2): left 6, right 11", the known rectsplit-r4 `table[col][row]` vs
`[row][col]` convention clash already in the ledger. This lane touched neither
`NZ_MAP_CTX_OFFSET_*` nor `base_ctx_rect`.

New/changed tests, all green: `filter_intra_classes_carry_their_own_libaom_default_row`,
`a_pinned_aomenc_16x8_strip_reads_its_use_filter_intra_flag`; the two pixel arms are among
the 29 ignored, each carrying its measurement in its doc.
