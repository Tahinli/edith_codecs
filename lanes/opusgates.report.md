# Lane opusgates: ec-opus passes clippy, malformed input cannot panic it

Goal (DONE.md "ec-opus"): ec-opus passes `cargo clippy --all-targets -D warnings`, and malformed
input cannot panic the decoder.

Base sha: `95bc8a76` (the lane branch was cut from it). Note: the main checkout's HEAD had moved to
`9eca2864` by the time of the final check; the lane has not been rebased onto it.

## What changed

### 1. approx_constant (analysis.rs:579, :602)
The literal `1.442_695` is libopus's truncated log2(e), used verbatim in `analysis.c` for
bit-exact band energies. Kept it, moved it to `const LIBOPUS_LOG2_E: f32` with
`#[allow(clippy::approx_constant)]` and a one-line reason.

### 2. Other clippy lints in ec-opus (`--all-targets`)
- analysis.rs: six `excessive_precision` literals rewritten to their shortest f32 round-trip form
  (same f32 value, computed with Python `struct`); `let activity;` late init removed;
  `prev_bandwidth >= b+1` written as `> b`; `!(e < 1e9) || e.is_nan()` kept (NaN must take the
  branch) with `#[allow(clippy::neg_cmp_op_on_partial_ord)]` and reason.
- celt.rs: `c as i32 + 0` -> `c as i32`.
- celt_enc.rs: `l1_metric` loop to slice iterator; `#[allow(clippy::too_many_arguments)]` on
  `encode` and `dynalloc_analysis` (libopus-shaped signatures); six index loops that index parallel
  arrays get `#[allow(clippy::needless_range_loop)]` with a reason; the `#[cfg(test)] mod tests`
  moved to the end of the file (`items_after_test_module`).
- silk_enc.rs: `if CONST { if let .. }` collapsed into a let-chain (edition 2024).
- stack_budget.rs: a literal `{}` argument inlined; `format!` in `assert!` args inlined.
- tests/conformance.rs (13 lints): `if let` chain in `shellexpand`; index loops allowed with reason
  (6 sites); `type Row` alias; `.clamp(0, 3)`; redundant `&` in `format!`; a blank `///` line to
  fix a doc list.
- Clippy also found 20 lib + 3 lib-test lints before the conformance target was reached; all fixed.

### 3. unwraps in celt.rs (were :2227, :2297, :2406, :2410)
Infallible, not input-reachable: `quant_band` sets `y = Some(..)` only together with
`split = true` (line ~2122), `y` is never cleared, and `stereo = y.is_some()` is fixed on entry.
Every `y.unwrap()` sits under `stereo` or `split`. Replaced with `.expect(INV_SIDE)`, where
`INV_SIDE` names this invariant (`const INV_SIDE` next to `DECODE_BUFFER`). The `:2410` site
also hoisted into a single `let ys`.

Other panic-capable sites in the decode path, reviewed:
- `range.rs` `debug_assert!`s at :209 (`dec_uint` ft > 1), :475 (`enc_uint`), :492 (`enc_bits`),
  :513 (`shrink`). Release builds elide them. Not fixed; the decode-side `ft` values come from
  constants in the callers I did not trace. Unverified: whether any input-derived value reaches
  `dec_uint` with ft < 2 (would divide by zero or misbehave).
- `packet.rs`, `multistream.rs`, `lib.rs`, `silk.rs`, `ogg.rs`: no `unwrap`/`expect`/`panic!` outside
  `#[cfg(test)]` on the decode path. Indexing was not audited exhaustively; the fuzz test (item 4)
  is the evidence for it.

### 4. New test: `crates/ec-opus/tests/malformed_input.rs`
Three tests, fixed-seed LCG (MMIX constants, seeds `0x5eed_0bad_cafe_f00d`, `0x7247_4e43_4154_4544`):
- `random_packets_never_panic`: 1000 random byte strings of length 1..=1500, random rate in
  {48k,24k,16k,12k,8k}, random channels 1..=2.
- `truncated_packets_never_panic`: 1000 random packets cut at a random length, decoded as mono
  and stereo at 48 kHz.
- `empty_and_single_byte_packets_never_panic`: empty packet, and each of the 256 TOC bytes alone.
Every decode runs under `catch_unwind`; a panic fails with the packet's hex. No `#[ignore]`.
The random TOC byte reaches SILK, CELT and hybrid paths.

## Verify

Commands run through `scripts/remote-run.sh opusgates -- ...` from the worktree.

| Command | Result |
|---|---|
| `cargo clippy -p ec-opus --all-targets -- -D warnings` | FAIL, but only in dependency `ec-dsp` (`crates/ec-dsp/src/lib.rs:189`, `clippy::chunks_exact_to_as_chunks`). ec-dsp is not touched by this lane. |
| same, plus `-A clippy::chunks_exact_to_as_chunks` | PASS (exit 0), zero ec-opus diagnostics |
| same command run on main (`/home/user/Documents/Code/Rust/edith_codecs`, lane `opusgates-main`) | FAIL, identical `ec-dsp` error. So the failure predates this lane. |
| `cargo test -p ec-opus --release` | PASS. lib 42 passed; conformance 28 passed, 16 ignored (library gate, needs ffmpeg and ~/Music corpus; pre-existing `#[ignore]`); malformed_input 3 passed; doc-tests 0. |

The unfixed `ec-dsp` lint blocks the exact gate. Fixing it means editing `crates/ec-dsp/src/lib.rs`,
which is another lane's crate; left alone.

## Left
- The `-D warnings` gate needs the `ec-dsp` lint fixed (or an `#[allow]` added there) on main.
- The `dec_uint`/`enc_bits`/`shrink` `debug_assert!` guards are not `Err` returns; no input-to-ft
  trace was done.
- The fuzz test found no panic on the current tree, but it is a sample of 3000 inputs plus 512
  single-byte TOCs, not a proof.
