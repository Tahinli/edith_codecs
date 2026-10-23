# av1splitstream report

**Goal:** mechanical split of `crates/ec-av1/src/stream.rs` (57,178 lines) into modules, zero behaviour change.
**Base:** main @ 322837a2 (lane start), then merged main @ 4872ca8e (av1splitdec).
**Branch:** lane/av1splitstream, head 512d7ea2.

## Steps done
- S1: `stream.rs` -> `stream/mod.rs` (git mv, 0 lines moved).
- S2: shared test helpers -> `stream/tests/support.rs`; gate tests -> `gates_a.rs`, `gates_b.rs`. Helpers, statics and FrameFacts fields made `pub(super)`; `lock_gate_counters` re-exported from `tests` for `encoder.rs`.
- S3-S12: the 397 `#[test]` fns moved in ten chunks: `t01.rs` ... `t10.rs`. `tests/mod.rs` is now a header (module list and imports, 20 lines).
- S13: fallout fix after the merge: child-process test filters now name `stream::tests::t09::...`; `test_source::read_module` also normalizes indented `    pub(super) fn` (so the source scanners see helpers as before the split).
- S13b: `gate_coverage::print_tx_size_search_census` ignores glued non-test helper segments when checking for unresolved tx-size-search values.
- Merge of main (av1splitdec): `stream.rs` deleted on this branch (the split version lives in `stream/mod.rs`); main's scanner change (`flat_of("decode_inter_block_intra_arm")`) applied to `stream/tests/t01.rs`.

## Line counts (after this lane)
| File | Lines |
|---|---:|
| stream/mod.rs | 2330 |
| stream/tests/mod.rs | 20 |
| stream/tests/support.rs | 4195 |
| stream/tests/gates_a.rs | 2850 |
| stream/tests/gates_b.rs | 2813 |
| stream/tests/t01.rs ... t10.rs | 4304 to 4594 each |

Every file is under 5,000 lines.

## Verify
- `cargo check -p ec-av1 --tests`: clean (host b, earlier; also re-checked after S12).
- `#[test]` attribute count: 398 in `stream/` before and after the split (original `stream.rs` also had 398).
- Full gate (host c; host b was held by av1splitdec, and the orchestrator allowed c):
  `EC_MEMGUARD_MAX=6G cargo test --release -p ec-av1 -- --test-threads=4`
  Result: **807 passed, 0 failed, 49 ignored** (lib), doc tests 4 passed, 0 failed.
  Earlier runs before the fallout fixes: 799 passed / 8 failed (child filters and scanner), then 806 / 1 (census helper). Both were fixed as described above.

## Left
- Unused-import warnings from the split are cleaned up for the stream files touched.
- Nothing else open for this lane.
