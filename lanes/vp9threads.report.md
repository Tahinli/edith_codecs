# lane vp9threads: report

Base: `bdc74c55` plus a merge of main. Branch `lane/vp9threads`.

## Status

| Goal | State |
|---|---|
| (a) fixture tests run, conflict resolved | Done. `fixture_dir` panics under `EC_REQUIRE_FIXTURES` when fixtures are absent. |
| (b) libvpx profile 0 and 2 vectors bit-exact | Met. 305 of 305 vectors clean, 4766 shown frames, 0 problems. `KNOWN_FAILING` is empty. |
| (c) tile threading, 1080p30 realtime | Not worked on (no speed work this round). Tile threading is deterministic: raw output at 1, 2 and 4 threads is identical. About 16-20 fps at 1080p on 4 cores from the earlier measurement. |

## Conformance rounds (libvpx vp90-2-* and vp92-2-*, per-frame md5)

Scope: profile 0 (8-bit 4:2:0) and profile 2 (10-bit 4:2:0). The test reads the profile from the vector name (`vp90`/`vp92` run; `vp91`/`vp93` are refused by name as Frozen and counted as skipped; the fixture set holds none).

| Round | Change | Clean / total | Frame problems |
|---|---|---|---|
| 1 | baseline | 109 / 305 | 944 |
| 2 | skip promotion for inter blocks with no coefficients; MV class count off by one | 276 / 305 | 780 |
| 3 | unscaled MC takes no MV clamp; error-resilient frames clear the previous segment map; segment map saved from intra frames and kept across frames with segmentation off | 305 / 305 | 0 |

Per profile after round 3: profile 0 (`vp90-2-*`) 303 / 303, profile 2 (`vp92-2-*`) 2 / 2.

Root causes, all found with a read-trace and block-dump oracle built from libvpx 1.13.1:

1. `adapt.rs` `inc_mv_component`: the MV class loop used `2 << (c + 2)`; libvpx `mv_class_base` is `2 << (c + 3)`. Wrong counts changed the adapted MV class probabilities and desynced later frames (sharpness-1 and most multi-frame vectors).
2. `decode.rs`: libvpx sets `mi->skip = 1` on an inter block of size 8x8 or larger whose `eobtotal == 0`. Later skip contexts and loop filter masks see the promoted flag. `reconstruct_inter_pixels` now returns that fact to the caller.
3. `mc.rs` `build_inter_predictors`: libvpx 1.13.1 clamps the MV (`clamp_mv_to_umv_border_sb`) only on the scaled path, and then against the whole block width and height. We clamped every path against the 4x4 sub-block, which broke sub-8x8 blocks whose MV points far outside the frame (`vp90-2-02-size-*`).
4. Segmentation map lifetime: libvpx zeroes `last_frame_seg_map` on error-resilient frames (`vp9_setup_past_independence`) and swaps current and last map only when segmentation is enabled. We never stored the map of intra frames, never cleared it for error-resilient inter frames, and replaced it with zeros when a frame had segmentation off. This covered `segkey_adpq`, `skip-02` and all 24 `resize_inter_*` vectors (they are error-resilient with temporal map prediction).

## Verify (host a, `scripts/remote-run.sh vp9threads --host auto`)

- `cargo clippy -p ec-vp9 --all-targets -- -D warnings`: exit 0.
- `EC_REQUIRE_FIXTURES=1 cargo test --release --no-fail-fast -p ec-vp9`: exit 0, 0 failed in every target; `libvpx vectors: 305 checked, 4766 shown frames, 0 problems`.
- `tests/scratch_interdump.rs` is deleted. It parsed `/tmp/inter1.ivf`, a tmpfs stream cut from a clip that is not in the repo (pinned sha256 376532...8fa8), so it failed under `EC_REQUIRE_FIXTURES=1` on every host. The 305-vector md5 test covers the same parse path with committed-by-reference fixtures.

## Test changes

`tests/libvpx_vectors.rs`: profile filter, `KNOWN_FAILING` (name, first divergence; a listed vector that starts passing fails the test, an unlisted one that fails fails the test), first problem logged per vector, `EC_VP9_ONLY=<substr>` and `EC_VP9_DUMP=<dir>` hooks for single-vector debugging.

## Leftovers

- No speed work (1080p30 not reached).
- Dead-code removal and superseded-header items in DONE.md were already satisfied on main.
- `scripts/remote-run.sh` has local changes by another lane and is excluded from this commit.
