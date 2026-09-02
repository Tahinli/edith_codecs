# lane-intra14 r4 -- six merges, the film witness located and pinned, blocked by a sibling refusal

Branch `lane-intra14`. The r3 blocker ("no aomenc source codes an intra block
on a 1:4 partition inside an inter frame") is **closed as a search**: the shape
IS carried by the user's 2160p 10-bit film, it is now pinned as a tracked
fixture with the counter attributed to a single decode-order frame, and the
gate that decodes it is written. It stays `#[ignore]`d for one measured reason
that belongs to another lane: that frame stops at "a split intra strip whose
transform unit is 64x32".

## 1. Merges (bc41417, 5d685f2, d8967dd, 4a449fd, d7ce5b3, e6284fd)

`main` 1176a16 then, in order, `lane-intersub8 a5b9770`, `lane-uv8 7f372b9`
(carries `lane-interp3 10b801a` -- "Already up to date" for interp3 itself),
`lane-inter16ab 2e711e1`, `lane-rectchroma2 48216c2`, `lane-sb128c fedb7fe`.
Resolutions that were decisions, not text:

| conflict | resolution |
|---|---|
| `read_intra_mode_rect` (decode.rs:5853) | main's `read_intrabc_info` read placed AFTER this lane's `skip` passthrough, then this lane's inter-vs-kf `y_mode` split |
| OBMC sentinel refusal wording | kept lane-uv8/interp3's ("no switchable symbol for that block"), inventory string moved with it |
| `masked_compound_used_wh` vs `is_any_masked_compound_used_here`, `bsize_index`+`wedge_used_wh` vs `bsize_all_index`+`wedge_used_bsize` | kept lane-inter16ab's pair -- it is the branch that landed the RECT wedge codebooks |
| the auto-merged `if write_w != write_h { return Err("a COMPOUND_WEDGE mask on a non-square inter block") }` | DELETED: it sat in front of lane-inter16ab's rect codebook lookup and would have made it dead code (class refusal-short-circuits-its-own-code) |
| `TxbSet::LumaRect8x4Inter` defined by BOTH intersub8 and inter16ab | one copy kept, both `*Set1`/`*1` siblings kept |
| 16x16-level AB `inter_leaf8!` macro vs sb128c's `if sb128_none` 128-root arm | both kept, macro first, one added closing brace |
| `vartx_leaves` LF fill | sb128c's `skip_inter_128` suppression prelude with lane-golomb's luma-only leaf comment inside |

Three refusal strings were deleted because the merged tree no longer contains
them (the inventory test names each one): "an inter partition below 8x8",
"a COMPOUND_WEDGE mask on a non-square inter block", "a 1:4 rect strip that
actually uses a palette".

## 2. Film sweep -- 27 segments, where the shape actually lives

`ffmpeg -ss <t> -t 2 -c:v copy -f obu` of the 3840x1608 10-bit AV1 HDR10 film,
each decoded by `decode_probe` under a 6G scope with `EC_AV1_FINAL_DUMP`
(frames = completed frames actually dumped, hits = `intra_rect4_in_inter`):

| ss | frames | intra 1:4 in inter | stop |
|---|---|---|---|
| 0, 2, 5, 10 | **33** | 0 | split intra strip TU 64x32 |
| 20, 60, 600, 2700, 6000 | 1 | 0 | split intra strip TU 64x32 / 32x64 |
| 30, 150 | 1 | 0 | intra 8x4/4x8 in a sub-8x8 inter partition |
| 45, 240, 900, 1200, 1800, 2100, 3300, 3600, 4200 | 1 | 0 | split (nonzero tx_depth) transform on an intra HORZ/VERT strip in an inter frame |
| 1500, 2400, 5400 | 1 | 0 | non-skip rectangular strip needs rectangular residual coding |
| 300 | 1 | 0 | split intra strip TU 32x64 |
| **90** | 1 | **16x64=3** | split intra strip TU 64x32 |
| 3000, 4500 | 1 | **16x64=4** | split intra strip TU 32x64 / 64x32 |

The charter's premise that the `-ss 0` extract decodes 30 frames REPRODUCES
(33 frames here) -- and that prefix codes the shape zero times. Every segment
that DOES code the shape stops at its first inter frame.

Attribution at ss=90, by truncation (`census4/trunc.py`, frames=1/2/3):
`n=1` decodes OK with hits 0/0/0/0; `n=2` and `n=3` both refuse with
`16x64=3`. So the three intra 16x64 strips are inside decode-order frame 1,
which is exactly the frame that refuses -- no completed frame in the whole
sweep contains the shape.

EVIDENCE: /tmp/claude-1000/-home-tahinli-Documents-Code-Rust-edith-codecs/b6d8a07f-96a4-4bbb-b378-af9ae25cf7c9/scratchpad/r4/probe*.log | 27 x (ffmpeg -ss N -t 2 -c:v copy -f obu, decode_probe under systemd-run --scope MemoryMax=6G, EC_AV1_FINAL_DUMP frame count) + a 3-point truncation ladder at ss=90 | 33 frames decode at the head with 0 hits; ss=90/3000/4500 fire 16x64=3/4/4 and every one of them is inside the first refusing inter frame

## 3. The witness fixture and its gate (e04736b)

`crates/ec-av1/fixtures/hg_intra14_witness.obu`, 163057 bytes, sha256
`0eff603bf1608e47faf5e6729670c4c77cf5c674dbad3a1533ac6660151fd90e` -- the ss=90
extract truncated to its first two decode-order frames. Extracted TWICE from
independent `ffmpeg` runs; both hashed identically (class
seeded-fixture-not-reproducible).

Gate `a_10bit_film_inter_frame_with_intra_1to4_strips_decodes_pixel_exact`
(stream.rs, next to `intra_rect4_in_inter_gate`): decodes the fixture, HARD-
asserts `intra_rect4_in_inter_counters()` moved, asserts 2 frames, and compares
EVERY plane of EVERY frame against `ffmpeg_decode_sequence_10bit` -- the same
helper `the_hunger_games_ss300_key_frame_skipped_cfl_decodes_pixel_exact` uses.
It is `#[ignore]`d with the blocking refusal quoted in the attribute: the
fixture's inter frame stops at "a split intra strip whose transform unit is
64x32 (no luma coefficient tables for that shape here)", a SIBLING lane's
surface (TX_64X32/TX_32X64 luma coefficient tables), not this lane's shape.
Un-ignoring is a one-line change once those tables land.

Forced run of the ignored gate (`--include-ignored --nocapture`) -- the wiring
proof that the fixture reaches the reader and stops on the named sibling
surface, not on anything of this lane's:

```
a_10bit_film_inter_frame_with_intra_1to4_strips_decodes_pixel_exact: decode_stream refused:
  unsupported: AV1 tile (a split intra strip whose transform unit is 64x32
  (no luma coefficient tables for that shape here))
test result: FAILED. 0 passed; 1 failed; 0 ignored; 442 filtered out; finished in 27.98s
```

EVIDENCE: crates/ec-av1/fixtures/hg_intra14_witness.obu (163057 B, sha256 0eff603bf1608e47faf5e6729670c4c77cf5c674dbad3a1533ac6660151fd90e) | two independent ffmpeg -ss 90 -t 2 extracts truncated to 2 frames, hashed identically; cargo test -p ec-av1 --lib -- a_10bit_film_inter_frame_with_intra_1to4_strips_decodes_pixel_exact --include-ignored --nocapture | forced run stops at the sibling refusal "split intra strip whose transform unit is 64x32"; the 3 intra 16x64 strips are in the same (refusing) frame

## 4. Suite -- RED, 2 failures, both inherited with the merges

`cargo test -p ec-av1 --lib` as a user systemd unit (MemoryMax=10G):
**404 passed; 2 failed; 36 ignored**; 1049.20s (r3 was 382/0/35 on a tree with
none of these six merges).

| failing test | first sample | owner |
|---|---|---|
| `a_frame_edge_straddling_band_decodes_pixel_exact` | 192x68 cq61 frame 1 Y row 0 col 64, ours 56 vs ffmpeg 178, 6859 px | lane-tile2's known 192x68 tile_cols=1 regression (ledger dead-end: cross-tile temporal MV reads already exonerated) |
| `a_real_aomenc_10bit_inter_sequence_with_a_split_transform_intra_block_decodes_pixel_exact` | seed 54 frame 4 Y (48,16) 109 vs 303, 2341 samples; split intra-in-inter 16x16=5 | a merged sibling's surface (16x16 split-transform intra-in-inter), not the 1:4 shape -- this lane's reader fires 0 here |

Neither failure touches the 1:4 shape (`intra_rect4_in_inter` is 0 in both) and
neither is reachable from this round's diff other than by having merged the
branch that carries it. Not fixed here -- outside this lane's shape and not
small.

EVIDENCE: $HOME/.cache/intra14-suite-r4.log | systemd-run --user --unit=intra14-suite-1788348669 -p MemoryMax=10G, EC_NOMEMGUARD=1 EC_AV1_REQUIRE_AOMENC=1, nice -n 10 -j3 | test result: FAILED. 404 passed; 2 failed; 36 ignored; 1049.20s

## Residue

* deferred(TX_64X32/TX_32X64 luma coefficient tables for a split intra strip):
  un-ignore `a_10bit_film_inter_frame_with_intra_1to4_strips_decodes_pixel_exact`.
  That single refusal is all that stands between this lane and a live,
  pixel-compared, counter-asserted witness -- the fixture is pinned and the
  gate is written.
* deferred(lane-tile2 / the split-transform intra-in-inter lane): the two suite
  failures above, inherited from the six merges, each reproduced with its first
  differing sample recorded.
* accepted: no aomenc recipe was hunted this round -- 27 film segments settled
  the question the recipe sweep was proxying for (the shape exists only in the
  film's inter frames, and every such frame refuses earlier).
