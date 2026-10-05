# lane/av1stale422prose — honesty sweep of stale 4:2:2 sequence-header-refusal prose

Base: `6d646f65`. Branch `lane/av1stale422prose`, worktree
`/home/tahinli/.cache/wt/av1stale422prose`. Not pushed, not merged.

The 4:2:2 sequence-header refusal is already lifted on this base. The live guard
(`stream.rs`, near the old guard block) DECODES 4:2:2 `(1,0)` and asserts only the
uncodable 4:4:0 `(0,1)` cell (`the_440_cell_is_not_a_codable_chroma_shape`). This
lane is **comment-only**: it rewrites prose a current reader would take as "4:2:2
still refuses / only a bypass can reach it", and leaves every behavioural assert
untouched. `decode.rs` was not edited.

## Diff proof: comment-only

```
$ git diff -U0 | grep -E '^[+-]' | grep -vE '^(\+\+\+|---)' \
    | sed 's/^[+-]//' | sed 's/^[[:space:]]*//' | sort -u | grep -v '^//'
(empty)   # every added/removed line is a // or /// comment

$ git diff --stat
 crates/ec-av1/src/refusal_inventory.rs |  4 +-
 crates/ec-av1/src/stream.rs            | 80 ++++++++++++++++++++--------------
 crates/ec-av1/src/tile.rs              |  9 +++-
 3 files changed, 56 insertions(+), 37 deletions(-)
```

`cargo check -p ec-av1 --tests` — clean (Finished, 0 warnings from this crate).

Short non-encode gate run:
`the_440_cell_is_not_a_codable_chroma_shape` — ok (1 passed).

## Rewritten sites

### 1. `stream.rs` — palette-window gate doc (`the_pinned_422_palette_intra_in_inter_cell_window_is_byte_exact`)
- OLD: "While `decode_stream` still refuses 4:2:2 at the sequence header (`decode_frame`, `subsampling_x != subsampling_y`) that is the refusal BY NAME … When the header lift lands the same test becomes the full gate …"
- NEW: "lane-av1422lift: `decode_stream` no longer refuses 4:2:2 at the sequence header, so the OK arm is the live one — the gate decodes the pin and asserts 17 decode-order frames byte-exact against aomdec with `palette_422_unit_window_hits()` fired. The `Err` arm below is the stale half: it still compiles in the old refusal string (`REFUSAL`, `"a chroma format of 4:2:2"`) and a counter-stays-put assertion. That is code, not a comment, and this prose sweep deliberately leaves it alone."

### 2. `stream.rs` — intrabc 128rect chunk-walk source-scan arm doc
- OLD: "The decode-level half of the red/green evidence cannot run in committed code (the 4:2:2 refusal stands), so this arm pins …"
- NEW: "The decode-level half of the red/green evidence now runs in committed code too — the witness gate above, `the_pinned_422_intrabc_sb128_strip_witnesses_decode_pixel_exact`, decodes both pins byte-exact against ffmpeg (lane-av1422lift lifted the sequence-header refusal). This arm complements it by pinning …"

### 3. `stream.rs` — lossless-inter witness doc (`the_pinned_422_lossless_inter_witnesses_decode_pixel_exact`)
- OLD header: "… the three 4:2:2 LOSSLESS INTER witness fixtures, PINNED, and asserted to refuse at the SEQUENCE HEADER."
- NEW header: "… PINNED. As of lane-av1422lift they are asserted to DECODE pixel-exact against ffmpeg … before the lift this gate asserted the SEQUENCE-HEADER refusal instead."
- OLD body: "WHAT THIS GATE DOES NOT COVER … This gate is therefore VACUOUS … Nor can any committed gate witness that fix's DECODE behaviour: with the header refusal standing, no committed test reaches a 4:2:2 decode, because the `EC_AV1_ALLOW_422_PROBE` bypass is a patch-run-restore hack that must never be committed …"
- NEW body: "WHAT THIS GATE DID NOT COVER BEFORE THE LIFT … As a refusal contract the gate was therefore VACUOUS … That refusal is GONE (lane-av1422lift lifted the sequence-header refusal), so the decode-level half of the evidence now runs in committed code: this gate's own byte-exact arm below decodes all three fixtures against ffmpeg. `EC_AV1_ALLOW_422_PROBE` was only ever the patch-run-restore bypass for the manual measurement in `lanes/av1422lpf.report.md`; it is still deliberately not committed …"

### 4. `stream.rs` — residual compound-warp witness doc
- OLD: "… it is deliberately NOT committed, and the refusal below is what the committed tree asserts):"
- NEW: "… it is deliberately NOT committed). This block is the bypassed measurement; the committed assertion is now the byte-exact compare in the lane-av1422lift paragraph below:"

### 5. `stream.rs` — LR-off (`lrless`) witness doc
- OLD: "… deliberately not committed, and the refusal asserted below is what this tree ships):"
- NEW: "… deliberately not committed). This block is the bypassed measurement; the committed assertion is now the byte-exact compare in the lane-av1422lift paragraph below:"

### 6. `stream.rs` — 128x128 none-root inter gate doc
- OLD: "… is witnessed in `lanes/av1444128.report.md` against the merged 444 tree — this tree still refuses non-4:2:0 sequences by name."
- NEW: "… is witnessed in `lanes/av1444128.report.md` and committed here as `a_real_aomenc_444_intra_in_inter_128_root_codes_chroma_per_mu_chunk_unit_pixel_exact`. lane-av1422lift: the sequence-header refusal is gone, so this tree no longer refuses non-4:2:0 sequences by name."

### 7. `stream.rs` — 12-bit 4:4:4 inter gate doc
- OLD: "… still 4:4:4 planes, and the decoder's refusal only rejects `subsampling_x != subsampling_y`."
- NEW: "… still 4:4:4 planes. lane-av1422lift: 4:2:2 (1,0) is DECODED now, so the only subsampling the decoder still asserts against is the uncodable 4:4:0 (0,1) cell, closed by `the_440_cell_is_not_a_codable_chroma_shape`."

### 8. `tile.rs` — `write_inter_block_128` mu-chunk writer PORT note
- OLD: "… before 4:2:2 could be written here. Refused by name at the sequence header today, so this is a PORT note, not a live defect."
- NEW: "… before 4:2:2 could be written here. This is a PORT note, not a live defect, because nothing feeds this writer 4:2:2: every non-test `ColorConfig` the encoder names is (1,1), pinned by arm 2 of `the_chroma_palette_map_side_is_the_420_plane_block_and_nothing_else_reaches_it`. lane-av1422lift lifted the decoder's sequence-header refusal, so the reason this is not live is the writer's 4:2:0-only source, not a refusal."

### 9. `refusal_inventory.rs` — strip-domain enumeration comment
- OLD: "… so these three are the whole reachable set, and each is walked whether or not the sequence header still refuses it."
- NEW: "… so these three are the whole reachable set, and each is walked independently of any sequence-header guard (lifted by lane-av1422lift)."

## Historical-and-left (already name the lift, or describe a different refusal)

- `stream.rs` `sb128rect_chroma_replay_counters` doc (~323): explicitly says the old prose "was TRUE. 4:2:2 is decoded now …" — correct.
- `stream.rs` ~1801 (the old-guard block comment): records the lift and the (0,1) assertion — correct.
- `stream.rs` ~2644–2652 (`the_440_cell_is_not_a_codable_chroma_shape` doc): records that the old refusal assertion "was honest only because the decoder refused EVERY (1,0) header" — correct.
- `stream.rs` ~3087–3096 and ~3489–3502: carry a lane-av1422lift paragraph that states the lift; the bypass mention sits inside the labelled pre-lift measurement — historical.
- `stream.rs` ~7008, ~7129–7173, ~9200: the 4:2:0 **oracle helper** refusing a 4:2:2 pin — a helper-level refusal, still live and unrelated to the sequence-header guard.
- `stream.rs` ~2921–2945, ~3000–3025, ~50007, ~50092: `440_request_is_422` / `422_header_edge16_walk` refused at the **subsize** level (`BLOCK_INVALID`), the live uncodable-shape refusal — correct.
- `refusal_inventory.rs` ~515–524 and ~1755–1760: state the refusal is GONE at the lift — correct.
- `examples/gen_coverage_cells.rs:260`: "lane-av1422lift: the decoder no longer refuses it" — correct.

## Left alone deliberately (code, not comment)

- `stream.rs` `the_pinned_422_palette_intra_in_inter_cell_window_is_byte_exact`: `const REFUSAL: &str = "a chroma format of 4:2:2";` and the `Err`-arm `assert!(err.contains(REFUSAL), …)` plus its `eprintln!`. These are **executable** refusal-string assertions. Not flipped in this lane; the doc above them now says so.
