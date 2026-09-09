# lane-rectdq — the 128 rect root pays for the delta_q group it codes

Branch `lane-rectdq` off main 748e4328. Class name: **pricer omits a group the
writer codes** (sibling of `pricer-context-zero`: there the pricer priced the
right tree at the wrong context, here it priced a tree with a whole symbol
group missing).

## The defect

`crate::tile::write_delta_q` codes its symbol group for every block that sits
at its superblock's mode-info origin, with ONE exception: a block that is the
whole superblock AND is skipped (`if is_whole_sb && skip { return }`), which
mirrors decode.rs' `whole_sb = (side == write_w == write_h == sb_px)`.

So at a 128 superblock root:

| root shape | codes the delta_q group? | was it priced? |
|---|---|---|
| `PARTITION_NONE`, skipped | no | n/a (correct) |
| `PARTITION_NONE`, residual (`EC_AV1_B128RES`) | yes | NO |
| `PARTITION_HORZ`/`VERT` halves (first half only) | yes | NO |
| four 64 superblocks under a 128 grid (first cell) | yes | NO |

The rect arm and the split arm were both under-priced by the group's bits on
every root of every frame that arms a delta_q plan (preset 0 has objective tpl
delta_q on, `DQ_TPL_K`) — the rect arm was measured against a NONE arm that
really is free, so it looked cheaper than it is. The class sweep
(`grep -n write_delta_q crates/ec-av1/src/tile.rs`) found the four writer sites
beyond the two 128 ones (Whole64 at `!sb128_armed()`, the 32 square, and both
inter leaves); under a 128 grid every one of them passes `is_whole_sb = false`,
which is why the four-superblock arm is charged here too.

## The fix

* `encode::delta_q_bits(target, cur, res)` — the writer's own three pieces in
  the writer's order, priced through a `SymbolEncoder::pricer` rather than a
  sum of table entries, because the escape's literal bits are equiprobable
  SYMBOLS carrying the `EC_MIN_PROB` floor and cost a little over one bit each
  (class `price-the-narrowing-not-the-table`; a table-entry sum was 0.43 bits
  light at `abs = 20`).
* `encode::delta_q_bits_at_root(...)` — the charge at one 128 root: the target
  is the root's own `sb_q` cell, exactly the entry `write_delta_q` indexes.
  **Stated approximation:** the writer's `CurrentQIndex` (`plan.cur`) is not
  knowable in the search — it only advances at the roots that really code a
  group, and which those are is what this very comparison decides — so the
  PREVIOUS 128 root's target in raster order stands in for it (`base_q_idx` at
  the first root, which is also the writer's per-tile reset). Wrong only where
  a neighbouring root came out a skipped NONE block, and then only by the
  difference between two adjacent tpl cells.
* Charged in `search_root_128_rect` for the FIRST half only (the second half
  sits at a non-origin mi and codes no group — `write_inter_block_128_rect`,
  tile.rs:517), in `search_root_128` only when the block is NOT skipped, and on
  the four-superblock side of the root comparison.
* `EC_AV1_RECTDQ=0` is the attribution control (pre-lane pricing).

## Witness

`tile::tests::the_delta_q_pricer_matches_the_writer_over_the_whole_domain`
enumerates the domain — `delta_q_abs` steps -20..=20, both signs, `res = 4` —
running `write_delta_q` into a pricing coder and comparing its `bits()` account
against `delta_q_bits`. Agreement is exact (`< 1e-9`), plus the disarmed case.

    cargo test -p ec-av1 --release --lib -- --exact \
      tile::tests::the_delta_q_pricer_matches_the_writer_over_the_whole_domain

Result: ok, 1 passed.

## Gates (lower BD = better; control = `EC_AV1_RECTDQ=0`, arm = default)

12 frames, `encode::tests::bd_rate_screen_native`, all five rows
(`~/.cache/rectdq/s.log`):

| row | vs libaom ctl → arm | vs rav1e ctl → arm | wall ours ctl → arm |
|---|---|---|---|
| bars 1080p | -3.4% → **-3.5%** | -19.0% → **-19.1%** | 186.2s → 189.2s |
| bars 2160p | +8.4% → +8.4% | -14.0% → -14.0% | 160.2s → 181.4s |
| film A | +18.0% → +18.0% | -6.3% → -6.3% | 177.6s → 179.1s |
| film B | +23.0% → **+22.8%** | -3.5% → **-3.7%** | 155.9s → 138.5s |
| screen capture | +14.4% → +14.4% | -33.2% → -33.2% | 144.6s → 106.9s |

48 frames, `encode::tests::bd_rate_film_long_gop` (`~/.cache/rectdq/g.log`):

| row | vs libaom ctl → arm | vs rav1e ctl → arm | wall ours ctl → arm |
|---|---|---|---|
| film A (`EC_AV1_NATIVE_FILM=1`) | +21.9% → +22.0% | -8.6% → -8.5% | 655.6s → 698.9s |
| film B (`EC_AV1_NATIVE_FILM4K=1`) | +75.8% → **+75.7%** | +0.8% → **+0.7%** | 494.1s → 464.7s |

Wall: every row inside +15% (the +13% on bars 2160p and +6.6% on long-GOP film
A are contention — two gate units shared the box, and the arm's own film B rows
came out FASTER on both gates; the search does one extra pricer call per root).

**Keep rule, honestly:** not met as written. No row goes >=0.5 down. The arm is
BD-NEUTRAL: the largest move in either direction is 0.2 (12f film B, down on
both columns), long-GOP film B is 0.1 down on both, long-GOP film A is 0.1 UP
on both, screen is byte-identical. It ships default-on as a CORRECTNESS fix
(the pricer now prices the tree the writer writes, witnessed exactly), not as a
win; the b128hv "+0.1 on the 12-frame film rows" premise this lane was chartered
from is not explained by the missing group — the group is worth ~2-9 bits a
root against roots costing thousands.

## Pins

`encode::tests::the_encoders_own_streams_are_byte_identical_to_their_pins` PASS
at default and at `EC_AV1_SPEED=6` — the pins did NOT move (the pin fixtures
arm no delta_q plan, so `delta_q_bits_at_root` returns 0 there).
`encoder::tests::every_speed_preset_decodes_sample_exact_through_both_decoders`
PASS; `encode::tests::a_128_root_horz_and_vert_halves_decode_exact_through_both_decoders`
PASS (`~/.cache/rectdq/p.log`).

## Suite

Three detached lanes, all green (`~/.cache/rectdq/t.log`):

* `cargo test -p ec-av1 --release -j4 -- --skip stream::` — 346 passed, 0 failed, 39 ignored (460s)
* `... -- stream:: --skip 10bit` — 202 passed, 0 failed, 15 ignored (927s)
* `... -- 10bit` — 42 passed, 0 failed, 1 ignored (66s)
* `timeout 900 cargo check --workspace --all-targets -j4` — 0 errors, 0 ec-av1 warnings (the only warnings are the pre-existing ec-vorbis `decode_capture` and ec-opus ones).

## Deviations from the charter

1. The charter's gate commands omit `--ignored`; both BD tests are `#[ignore]`,
   so `--ignored` was added or they would have filtered out silently.
2. The 12-frame gate was run WITHOUT `EC_AV1_NATIVE_FILM*` (one run per arm
   covering all five rows) instead of two per-film runs: the same two film rows
   plus the screen row the keep rule asks for, at half the runs.
3. The charge is made on the four-superblock arm as well as on the rect arm
   (charter item 2's class sweep) — charging only the rect arm would have
   biased the comparison toward SPLIT, which codes the same group. Both are
   under the single `EC_AV1_RECTDQ` knob.
4. The unit witness lives in `tile.rs`'s test module (it needs the private
   `write_delta_q`), not in `encode.rs`.
