# lane-av1skipfix — convert skip-on-decode-error gates; audit fixture provenance

Branch `lane-av1skipfix` off `a21f3680`. Tests only: no file under
`crates/ec-av1/src/decode.rs` was touched, and no decoder logic changed anywhere.

## Defect 1 — four `..._decodes_pixel_exact` gates turned a decode error into a printed SKIP

Pinned as a set in `refusal_inventory.rs` (`GATES_THAT_SKIP_ON_A_DECODE_ERROR`).
Each one ran a single recipe and, on `Err`, printed `SKIP <name>: {e}` and returned
green. Converted to the attempt-loop shape already used elsewhere in `stream.rs`:
sweep several recipes, count a named refusal per attempt (never swallow it), ALWAYS
pixel-compare an attempt that decoded, and hard-assert the feature counter on a
pixel-compared attempt. Exhausting every attempt is a hard failure, not a skip.

| gate | counter | recipes | decoded+fired (green run) |
|---|---|---|---|
| `a_real_aomenc_filter_intra_stream_decodes_pixel_exact` | `filter_intra_hits` | 2 seeds × 3 cq | 2 |
| `a_real_aomenc_intra_stream_with_deblocking_decodes_pixel_exact` | `deblock_hits` | 2 seeds × 4 cq | 2 |
| `a_real_aomenc_inter_sequence_with_deblocking_decodes_pixel_exact` | `deblock_hits` | 2 seeds × 4 cq | 2 |
| `a_real_libaom_gradients_stream_with_cdef_decodes_pixel_exact` | `cdef_idx_hits` | 8 × (size, cpu-used, crf) | 2 |

All four run under `lock_gate_counters()` and report the three-bucket
`gate_buckets` summary, so the buckets sum to the attempts made.

### Measurements (this box, 2026-09-29, base `a21f3680`)

- **filter intra** — seed 42 cq 40 and seed 43 cq 40 read `use_filter_intra` 3× each,
  pixel-exact. seed 44 cq 45 and seed 45 cq 30 **decode without it** (delta 0), so the
  one fixed recipe was carrying the whole gate.
- **intra deblock** — cq 55/45/35/63 all decode pixel-exact with 48/64/64/64 deblock edges.
- **inter deblock** — cq 55/45/35/63 all decode 4 frames pixel-exact with
  208/224/160/208 deblock edges.
- **CDEF** — the old recipe was a **single 64×64 frame**, which gives libaom's cdef
  search exactly one superblock, so it can never write `cdef_bits > 0` and the gate
  never read a `cdef_idx` at ANY crf / cpu-used (0, 2, 4, 8 × crf 10–60, sizes
  64×64…256×128: delta 0 everywhere). `EC_AV1_CDEF_DBG` shows `apply_cdef` *does* filter
  there (nonzero `t`, `pri=11`/`pri=2`), but the gate's own firing proof is
  `cdef_idx_hits`, so its premise was unfalsifiable on the recipe it shipped. The
  recipe list is now multi-SB and multi-frame: 128×64/128×128 at 24 frames read
  2–8 `cdef_idx` and are pixel-exact against ffmpeg.

No recipe in any of the four families failed to decode for a capability reason, so
**no entry had to be kept**; the inventory list is now empty and stays an exact pin.

### Red-before (each mutation reverted after the run)

| mutation | result |
|---|---|
| `--enable-filter-intra=0` in the filter-intra recipe | `none of 6 attempts decoded a stream that read use_filter_intra` — FAILED |
| `--loopfilter-control=0` in the intra-deblock recipe | `buckets counted-exact=0 uncounted-exact=8`; `none of 8 attempts … with a deblocking edge` — FAILED |
| `--loopfilter-control=0` in the inter-deblock recipe | same, FAILED |
| original 64×64/1-frame CDEF recipe restored | `buckets counted-exact=0 uncounted-exact=8`; `no attempt of 8 read a cdef_idx literal` — FAILED |
| one `SKIP <name>: {e}` arm re-added to a converted gate | `gates_that_swallow_a_decode_error_are_declared` FAILED, naming that gate |

Note the mutation runs report `uncounted-exact=8`: the eight attempts still ran real
pixel compares, so the compare is load-bearing and the counter assert is what bites.

## Defect 2 — `a_real_aomenc_lossless_444_key_frame_decodes_sample_exact` reported green on a missing pin

Its pinned fixture `fixtures/ll444-lossless-key.obu` does not exist: not in git, not
in the root `fixtures/` directory on this box, and no script under `scripts/` produces
it. The `Err` arm was a bare SKIP, so the test finished in 0.05 s (its siblings take
seconds — it had decoded nothing) and the suite reported green.

The root `fixtures/` directory is listed in `.gitignore:2`, so a pin that lands there
can never be committed and a fresh clone can never have it; every `~/.cache/wt/*`
worktree lacks it for the same reason. Committing a fixture is therefore not available
as a fix, and the gate is not deleted. The arm keeps its skip but `EC_AV1_REQUIRE_AOMENC=1`
now turns it into a hard failure naming the missing path, the exact regeneration recipe
and the `EC_AV1_PIN_DIR` override; the skip line itself says the gate decoded nothing
and proves nothing. Smallest possible hunk: the arm only, no gate body below it.

## Fixture-generator audit (this lane's manifest)

| referenced path | on disk | generator |
|---|---|---|
| `fixtures/video/*.mp4/.mkv` | present | `scripts/gen-fixtures.sh` (deterministic ffmpeg) — OK |
| `crates/ec-av1/fixtures/*.obu` (48, committed) | present | **NONE** — provenance lives only in gate doc comments / lane reports |
| root `fixtures/*.obu` pins: `golden6-mismatch`, `golden7-forwarding-mismatch`, `lr-sgr-r7`, `golden4-pin` | present | **NONE**, and gitignored so not committable |
| root `fixtures/*.obu` pins: `ll444-lossless-key`, `golden3-pin`, `sbpart-pin` | **missing on this box** | **NONE** (`golden3-pin` and `sbpart-pin` gates are `#[ignore]`d — the honest shape) |

So of the ~55 distinct AV1 fixtures these tests name, **every `.obu`/`.av1` pin has no
generator script**; only the container fixtures do. The pin directory is also split
across two homes (`crates/ec-av1/fixtures/`, committed, 48 files) and the root
`fixtures/`, gitignored — and `pin_dir()` resolves to the gitignored one, so any pin
written there is un-reproducible on a fresh clone by construction.

## Commits

- `cd81750b` — stream: convert the four skip-on-decode-error gates to attempt loops
- `0b92c6da` — refusal_inventory: empty GATES_THAT_SKIP_ON_A_DECODE_ERROR
- `3c302798` — stream: a_real_aomenc_lossless_444_key_frame stops reporting green on a missing pin
