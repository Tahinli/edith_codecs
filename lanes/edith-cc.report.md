# Lane cc: embedded closed captions (CEA-608/708)

Branch lane-cc, head 63d80a5b, based on main 70128cf2 (main has since advanced to cda806cc with the HEVC-speed merges; not rebased).
Commits: 81ce26f3 (feature), 63d80a5b (engine tests: closed_captions takes &Path).

## What it does
- Lists CEA-608 (CC1) and CEA-708 (Service 1) caption tracks embedded in the picture of H.264, HEVC and MPEG-1/2 files, as subtitle tracks beside the container's own tracks (names `CC1 (608)`, `Service 1 (708)`).
- Captions are found by one cached read of every picture packet, done only for files that have a caption row (`subtitle::open_all` passes `wants_captions`), so ordinary opens do not pay the cost.
- Cue times are on the picture clock (microseconds from source time 0).
- mcp `media` tool lists the caption tracks as `subtitle` streams (codec eia_608/eia_708) after the container's streams, and `subtitles_import` imports them.

## Files changed (vs merge-base 70128cf2), 8 files, +584 / -2
- crates/engine/src/demux/captions.rs (new, 240 lines): presence probe, cached track extraction, TRACK_BASE numbering.
- crates/engine/src/demux/ts.rs (+51): `caption_triplets` walks picture packets via ec-cc extractors.
- crates/engine/src/demux.rs (+1): `pub mod captions;`
- crates/engine/src/subtitle.rs (+52): `of_media_with`, `of_captions`, caption-aware `open_all`.
- crates/engine/tests/closed_captions.rs (new, 207 lines): integration tests (takes &Path).
- Cargo.lock (+11): ec-cc package and engine dependency entry.
- crates/mcp/src/tools/media.rs (+21): caption streams listed.

## Peer-owned hunks (review these)
- crates/engine/Cargo.toml: +3 lines, adds `ec-cc = { path = "../../../edith_codecs/crates/ec-cc" }` with comment. Peer-owned engine manifest.
- crates/mcp/src/tools/media.rs: +21 lines in `other_streams` (mcp crate is peer-owned).
- No hunks in project.rs, playback.rs, text.rs, edith.rs, assets, crates/app, or MCP.md in the lane's own diff. The apparent app/MCP.md deletions in `git diff main..HEAD` are main's newer HEVC-speed commits missing from the lane base, not lane changes.

## Gate (server host c, --with-edith, release)
- cargo clippy --workspace --all-targets -- -D warnings: clean (no warnings, Finished).
- cargo test --release -p engine: 40 test binaries, 249 passed, 0 failed, 5 ignored. No FAILED/panicked lines, so the known h264/hevc mid-GOP seek failures did not occur in this run.
- Commits contain no co-author or Claude trailers (grep count 0).
- Tree clean; no fix commit needed.
