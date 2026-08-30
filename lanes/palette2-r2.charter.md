# lane-palette2 r2 — finish UV palette reconstruction

At 96fde87. **I merged main for you.** r1 hit its cap mid-edit, having just
said "now add `record_palette_uv` at the tail, near `record_palette_y`"; its
tree is committed verbatim at d2ea8e7 and **is unverified — it may not compile.**
Check that first, then continue.

Your charter is still `lanes/palette2-r1.charter.md`; read it. Step 1 is UV
palette reconstruction: the previous palette lane ported `palette_uv_mode` and
`palette_uv_size` but deliberately skipped `read_palette_colors_uv`, because the
refusal aborted the decode immediately and no later bit needed to stay in sync.
That reader now has to exist, and both chroma planes have to be reconstructed
from the palette and its index map.

Gate first, below `decode_stream`, hard-asserting the UV palette path actually
fired — a pixel match on a stream that never used a UV palette proves nothing
([[gate-blind-to-feature]]). `testsrc2` at plain default aomenc settings reaches
this case; that is how I found it. Then lift the refusal and update
`refusal_inventory.rs` in the SAME commit.

Steps 2 and 3 (palette on a HORZ/VERT rect strip, palette with a split luma
transform) stay in the charter for a later round — do not start them until UV is
green and committed.

## Budget discipline
75 turns, no reset on resume. At about turn 55, stop starting new work: commit
what is green and write your report. The merge is already done for you.

Hard rules: `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-palette2`; foreground
`nice -n 19 cargo ... -j4`; every `cargo test` a timeout >= 600000 ms; ffmpeg
generates bounded with `-t`; `EC_AV1_REQUIRE_AOMENC=1`; aomenc
`--threads=1 --row-mt=0 --sb-size=64`. Oracle rung 12 is yours; `~/.cache/aom-oracle`
is SHARED, env-gated rungs only. Sibling worktrees have live agents — never build
in or edit them. Never push, never merge into main. End with
`lanes/palette2-r2.report.md`, VERDICT on line 1.
