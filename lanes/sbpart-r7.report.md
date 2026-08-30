VERDICT: PARTIAL -- checked r6's tree compiles clean (charter's order 0), built
oracle rung 11 (per-symbol range ladder inside `read_coeffs_txb`), and used it
to localize block2's real divergence to a specific coefficient `base` symbol
inside the luma corner scan -- a materially sharper answer than r5/r6's
`around_rect`/context-gather suspects, but did not reach or land a fix.

## 0. Compile check (charter's first step)

`cargo check -p ec-av1 --tests` on r6's committed-verbatim tree (137f09c):
clean, only pre-existing missing-doc warnings. No merge-conflict debris, no
signature mismatches. r6's tree was in fact fine; only its report write was
interrupted at the cap.

## 1. r6's own framing, re-checked

r6 proved block2's mode-info prefix (skip/cdef/dq/mode/angle_y/uv_mode/
angle_uv) is byte-exact, range-for-range, against the oracle -- and named
`around_rect` as the next suspect, since it feeds the coefficient reads'
`skip_ctx`/`dc_sign_ctx`. This round re-read `around_rect`/`around_mi_rect`
(decode.rs:2094-2118) and `decode_block_rect64`'s call site
(decode.rs:3214) against `decode_block_rect`'s own (already-working, per r5's
"sibling working" framing) `around_rect` usage: identical shape, identical
w/h-split gather pattern (above over the block's own width, left over its own
height), and the u/v skip_ctx formula (`above_coded as usize +
left_coded as usize`) is textually identical between the two functions. No
defect found there by inspection -- consistent with what the range ladder
below actually shows (skip_ctx and dc_sign_ctx both read correctly).

## 2. Regenerated the pinned mismatch (seed 42, same recipe as r2-r6)

`EC_SBPART_GATE_ATTEMPTS=1 EC_AV1_GATE_DUMP=<sp>/sbpart-r7-pin.obu
EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib
a_real_aomenc_stream_with_a_superblock_level_horz_vert_partition_decodes_pixel_exact
-- --nocapture`: reproduced the frame-0 luma mismatch on the first attempt,
pinned OBU captured fresh (scratchpad only, not committed -- per
[[oracle-in-reaped-dir]] these do not survive between sessions, re-capture
with the same command if needed).

## 3. Oracle rung 11: per-symbol coefficient range ladder

Rung 3 (`EC_TRACE_COEFF`, built by a sibling lane) only bracketed a whole
`av1_read_coeffs_txb` call -- entry/exit `rng`, nothing inside. That is
coarse enough to prove a block diverges but not where. Added rung 11 (same
env var, no new flag) in `scripts/instrument-aom-oracle.sh`, patching
`read_coeffs_txb` in `decodetxb.c` to print `EC_COEFF_STEP tag=<eob|
base_eob|after_bases|sign|post_golomb> ... rng=..` at four checkpoints:
right after `*eob` is computed, right after the special eob-1 base/br read,
right after the reverse-scan base/br loop for every other position, and
right after each sign/golomb read in the final forward loop. Applied to the
shared `~/.cache/aom-oracle` tree, `ninja -C ~/.cache/aom-oracle/build
aomdec`: clean.

Mirrored the entry/exit half on our own side: `EC_COEFF`/`EC_COEFF_VAL`
prints (same format) at all three coefficient-read call sites in
`decode_block_rect64` (luma `read_coeffs`, chroma U/V `read_coeffs_rect`,
decode.rs:3214-3320), plus `rng` added to the existing `EC_AV1_TRACE`
`eob_pt`/`eob_extra` lines in `read_eob` (decode.rs:1183,1190) for
comparability. `read_coeffs`'s own existing per-symbol traces
(`all_zero`/`tx_type`/`eob`/`base_eob`/`base`/`br`/`dc_sign`/`golomb`,
decode.rs:1261-1394) already carried enough to read off VALUES; this round
only needed the oracle side widened to carry `rng`.

Ran both traces on the identical pinned OBU
(`EC_TRACE_COEFF=1 aomdec --i420 -o /dev/null <pin>.obu`, and
`EC_AV1_GATE_DUMP_PIN=<pin>.obu EC_TRACE_COEFF=1 EC_AV1_TRACE=1 cargo test
--lib pinned_sbpart_stream_decodes_pixel_exact -- --ignored --nocapture`).

## 4. Result: block1 exact, block2's luma coefficient read is where it starts

Block1 (mi_col=16), all 3 planes, entry-to-exit rng: **byte-for-byte
identical** between ours and the oracle (luma 36949->41600, U 41600->42128,
V 42128->45272, all four numbers matching on both sides). Confirms r5's
pixel-exact finding down to the entropy level, not just pixels.

Block2 (mi_col=24) luma entry rng: **63764 on both sides** -- exact match,
confirming r6's mode-info-prefix finding held all the way through the
`skip`/`tx_depth` reads too (both zero symbols consumed identically, since
`tx_select=false` under `--enable-tx-size-search=0`). But the luma read's
**exit** range diverges: ours **46984**, oracle **46344**. This overturns
r6's framing that `around_rect` (feeding only `skip_ctx`/`dc_sign_ctx`) was
the leading suspect -- `all_zero` reads `value=0` (not skip) on both sides
and `dc_sign` reads the same value on both sides, so those two ctx values
were consumed correctly; the actual divergence is **inside the coefficient
magnitude loop**, after `eob` and before `dc_sign`.

Step-by-step inside block2's luma block (rung 11's checkpoints, both sides
agree on `eob=6` and the eob-1 symbol `base_eob level=1`):

```
scan position (row,col)   ours base value      oracle final level
pos=2  (row0,col2)  scan5  1 (base_eob)         1
pos=33 (row1,col1)  scan4  1                    1
pos=64 (row2,col0)  scan3  1                    1
pos=32 (row1,col0)  scan2  3 -> br(ctx8)=0 -> 3  1  <-- FIRST DIVERGENCE
pos=1  (row0,col1)  scan1  0                    ? (not individually logged
                                                  oracle-side; our value 0
                                                  means no sign is read for
                                                  it either way, consistent)
pos=0  (row0,col0)  scan0  3 -> br,br=3,0 -> 6   1  (downstream, corrupted
                                                       by the pos=32 read
                                                       already having
                                                       consumed the wrong
                                                       number/kind of bits)
```

The oracle decodes **every** non-eob coefficient in this block at level 1
(no `br` read ever fires) except the skipped zero at `c=3`. Ours decodes
`pos=32` (scan_idx=2, `row=1,col=0`) as base value **3** -- which is
`> NUM_BASE_LEVELS`, so it additionally reads a `br` symbol the oracle never
consumed at all, and `pos=0` (the DC, scan_idx=0) also comes out base value
3 with a further `br,br=3,0` (final level 6) where the oracle's DC is level
1. The DC divergence is very likely *downstream* corruption from the
scan_idx=2 desync (once one extra symbol is read, everything after it in
the reverse scan reads shifted bits); scan_idx=2 (`pos=32`, `row=1,col=0`)
is the first place values differ and is the actual desync point.

## Named suspect for next round

The symbol at `pos=32` is read via `read_coeffs`'s reverse-scan branch
(decode.rs:1338-1339): `let ctx = base_ctx(&levels, side, row, col, class);
let v = dec.symbol(&mut coding.base[ctx]) as i32;` -- our trace shows
`ctx=2` for this position. Since the entry range into the whole coefficient
loop and every earlier symbol (`eob`, `base_eob` at scan_idx 5/4/3`) all
matched the oracle exactly, the *decoder state* (msac range/value, CDF
adaptation counts) going into this read was identical on both sides -- so a
different decoded symbol value here can only come from a different **CDF
row** being read (wrong `ctx` from `base_ctx`, or `coding.base` indexed
under the wrong `TxbSet`/mode row) rather than a desync that already
happened earlier. `base_ctx` (decode.rs, search `fn base_ctx`) computes the
2D neighbour-magnitude context per spec `get_lower_levels_ctx`
(`Mag_Ref_Offset_With_Tx_Class`-style neighbour sum) -- next round should
hand-check `base_ctx`'s neighbour offsets and clamping for exactly
`(row=1, col=0)` in a 32-wide 2D-class scan against libaom's
`get_lower_levels_ctx` (`av1/common/txb_common.h`), and cross-check that
`coding.base` (the `cdfs.txb(TxbSet::Luma64, mode).base` table) is the same
CDF the oracle's `ec_ctx->coeff_base_cdf[txs_ctx][plane_type]` resolves to
for `txs_ctx = get_txsize_entropy_ctx(TX_32X64)` -- **this round did not
check** whether `TxbSet::Luma64`'s `txs_ctx` mapping matches libaom's for a
true `TX_32X64` (as opposed to a square `TX_64X64`); a mismatched `txs_ctx`
would select a differently-adapted CDF row even at the same numeric `ctx`
index and is a strong alternate explanation for a value divergence with no
range mismatch beforehand.

## What was committed (58a0d59)

Diagnostics only, no behavior change to the live gate (byte-identical
before/after this round's diffs to the gate's own logic; only tracing
statements added). `cargo check -p ec-av1 --tests`: clean. Oracle rung 11
applied to the shared `~/.cache/aom-oracle` tree and captured in
`scripts/instrument-aom-oracle.sh` (env-gated, idempotent, reproduces from a
fresh clone) per the charter's "never a throwaway patch left in the tree"
rule.

## Not attempted this round

- No fix landed; `sb_rect_hits() > 0` remains unproven (gate still red,
  unchanged from r6).
- Did not check `get_txsize_entropy_ctx`'s treatment of `TX_32X64` vs
  `TX_64X64` for CDF-set selection purposes (named above as the sharper
  alternate suspect to `base_ctx`'s neighbour math) -- ran out of budget
  before opening `cdf_state.rs`'s `txb`/`TxbSet::Luma64` resolution path.
- Did not extend rung 11 to chroma (`read_coeffs_rect`, untraced per-symbol
  on our side -- only entry/exit) since luma is where entry/exit already
  proved the divergence lives; chroma U/V exit ranges were not compared this
  round (moot until luma is fixed, since bits already shifted downstream).

## Hard rules followed

Worked only in this worktree; `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-sbpart`
every build; foreground `nice -n 19 cargo ... -j4`; `EC_AV1_REQUIRE_AOMENC=1`
on the aomenc-driven capture; aomenc recipe unchanged (`--threads=1
--row-mt=0 --sb-size=64 --enable-tx-size-search=0`, inherited); oracle rung
is env-gated (`EC_TRACE_COEFF`, no new flag), captured in
`scripts/instrument-aom-oracle.sh` not left as a standalone patch; no other
worktree touched; no push, no merge into main; killed a background
`cargo test` full-suite run started by mistake (unscoped `stream::tests::`)
before it could pile up, per turn-cap discipline.

## Next round, in order

1. Check `get_txsize_entropy_ctx(TX_32X64)` vs our `TxbSet::Luma64`'s own
   `txs_ctx`/CDF-set resolution (`cdf_state.rs`, search `Luma64`) against
   libaom's `av1/common/txb_common.h` `get_txsize_entropy_ctx` -- rule in or
   out before touching `base_ctx`'s neighbour math, since a wrong CDF-set
   selection produces exactly this symptom (same range in, different value
   out, no earlier range mismatch).
2. If that's clean, hand-check `base_ctx` (decode.rs) at `(row=1, col=0)`
   for a 32-wide `TxClass::TwoD` scan against libaom's
   `get_lower_levels_ctx` -- the neighbour offset table/clamping is the
   remaining suspect.
3. Fix, then re-run the full `n_attempts=40` gate and confirm
   `sb_rect_hits() > 0` stays a hard pass.
