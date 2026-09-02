# lane-wit16x4 r1 report (branch `lane-wit16x4`, off main 5d81a67)

## Verdict

The intra 16x4/4x16-in-inter lift had NO witness. The refusal is RE-ADDED
(narrow: only that shape, on the inter path, behind `EC_INTRA16X4_DECODE`).
The film gate is split and kept as the rect64 corner-TU witness only.

## 1. The pinned fixture, re-measured on this tree

`crates/ec-av1/fixtures/hg_rect64_intra16x4_witness.obu` (10-bit 3840x1608,
33 decode-order frames), `decode_probe`:

```
OK: 33 frames decoded, 3840x1608          diff vs ffmpeg: all 33 frames 0 bytes
rect64_corner_tu:      64x32=148 32x64=123      (r3 claimed 90 / 5)
intra16x4_in_inter:    16x4=0 4x16=0 chroma_ref=0   (r3 claimed 31 / 17 / 23)
intra_rect4_in_inter:  64x16=24 16x64=28 32x8=0 8x32=0
inter_rect:            64x32=1996 32x64=152 64x16=24 16x64=28
inter_edge_strip:      h64=1822 v64=0 h32=279 v32=0 h16=59 v16=0
rect_inter:            tu=429 txsplit=0 obmc_leaf=4
sub8_intra_rect:       0/0/0/0/0     inter16_1to4: 0/0/0/0
vartx_rect_leaf: 0/0   vartx_rect_leaf4: 0/0   rect32x8_inter_tu: 0/0
cdef_band: rect_skip_writes=1797 mixed_skip_units=0   rect_intrabc_reads: 0
```

So r3's two hard asserts were both wrong numbers off a desynced decode
(class `counter-from-refused-stream`): the rect64 arm fires ~1.7x more than
claimed, and the intra-16x4 arm does not fire at all.

## 2. Witness hunt for intra 16x4/4x16 in an inter 1:4 partition -- NONE

| stream | decode | intra16x4 16x4/4x16/chroma_ref | pixel |
|---|---|---|---|
| 2 s 10-bit 3840x1608 head cut | OK, 48 frames | 0 / 0 / 0 | (exact, frame36 r2) |
| same title, 300 s cut | REFUSED (Golomb tail), 8 frames | 158 / 603 / 379 | truncated to its 8 frames: 3 shown, frame 0 exact, frames 1/2 differ by 14105680 / 12470780 bytes -> counters are phantoms |
| 1920x792 128-SB cut @900 s | REFUSED (128x64 intra on inter path) after 1 frame | 58 / 68 / 64 | only the key frame decodes; no inter frame reachable |
| same @5400 s | REFUSED, 1 frame | 18 / 26 / 24 | same |
| same @6300 s | REFUSED, 2 frames | 187 / 560 / 365 | trunc(2): 1 shown frame, 0 differing bytes, counters 169/445/300 -- but trunc(1) (that same key frame alone) fires 0/0/0, so EVERY hit lives in the NO-SHOW frame 1, which no display frame reaches: trunc(3)/(4)/(5) all refuse on the 128x64 intra block (class `gate-blind-to-hidden-frames`) |
| same @8100 s | REFUSED, 1 frame | 0 / 4 / 2 | only the key frame decodes |
| aomenc gate `a_real_aomenc_inter_sequence_with_intra_16x4_strips_in_1to4_partitions_decodes_pixel_exact` | FAILED with `--include-ignored` | per-arm attempts `[0, 0, 0]` | 0 named refusals, 0 pixel-exact attempts carrying an intra 1:4 strip; attempts 1..3 mismatch ffmpeg from frame 1-2 (max abs delta 248), attempts 0/4 mismatch while carrying no 1:4 strip at all (another shape's defect, unchanged since intra16x4 r2) |

No exact, firing witness exists on this tree. Per class
`refusal-lifted-without-a-gate` the refusal goes back in.

## 3. Changes

- `crates/ec-av1/src/decode.rs` (`strip16` in the intra arm of the inter block
  path): the chroma-pair record that let a 16x4/4x16 strip past the refusal is
  now `.filter(|_| std::env::var_os("EC_INTRA16X4_DECODE").is_some())` -- the
  shape refuses by name again by default; the decode path (and its counter)
  stay wired for the round that finds a witness. Full measured note in place.
- `crates/ec-av1/src/decode.rs` (rect64 corner-TU comment): re-measured
  counters 148 / 123 and the new gate name.
- `crates/ec-av1/src/refusal_inventory.rs`: string unchanged, note records the
  re-widening and why.
- `crates/ec-av1/src/stream.rs`: gate renamed
  `a_10bit_film_frames_with_intra_16x4_strips_and_rect64_corner_tus_decode_pixel_exact`
  -> `a_10bit_film_frames_with_rect64_corner_tus_decode_pixel_exact`; hard
  assert kept on `rect64_corner_tu` both orientations, the intra-16x4 assert
  replaced by `assert_eq!(fired16, (0, 0, 0))` (a non-zero count means the
  refusal guarding that arm moved); doc block rewritten with the measured
  numbers.
- `crates/ec-av1/src/stream.rs` (aomenc 1:4 gate): stays `#[ignore]d`, reason
  replaced by this round's measured failure output.

## 4. Gates

Command: `EC_NOMEMGUARD=1 EC_AV1_REQUIRE_AOMENC=1 CARGO_TARGET_DIR=$HOME/.cache/cargo-target-wit16x4 nice -n 10 cargo test -p ec-av1 --lib -j3 <filter> -- --nocapture`

| filter | result |
|---|---|
| `a_10bit_film_frames_with_rect64_corner_tus` | ok, 1 passed, 63.37 s -- `33 frames pixel-exact, rect64_corner_tu 64x32=148 32x64=123 (intra16x4_in_inter 0 0 0, refused shape)` |
| `a_10bit_film_hidden_arf` | ok, 1 passed, 89.48 s (`rect64_split_txfm_publish=4`) |
| `refusal_inventory` | ok, 3 passed |
| `gate_coverage` | ok, 9 passed |
| `sub8` | ok, 7 passed, 35.16 s |
| `1to4` | 9 passed, 1 FAILED, 3 ignored (see below), 118.75 s |

The `1to4` red was `a_10bit_film_inter_frame_with_intra_1to4_strips_decodes_pixel_exact`,
which stopped at the re-added refusal. MEASURED before deciding: its fixture
`hg_intra14_witness.obu` has 2 frame OBUs and ONE shown frame; truncated to its
first frame OBU it decodes exact with `intra_rect4_in_inter 0/0/0/0` and
`intra16x4_in_inter 0/0/0`. So all of the 4/23/28/72 strips it asserts -- and
the 4 intra 16x4 strips with them -- live in the SECOND, NO-SHOW frame, whose
pixels ffmpeg never writes and the gate never compared (class
`gate-blind-to-hidden-frames`); the gate was green on a counter nothing backed.
It is `#[ignore]`d with that measured reason, to be un-ignored by a prefix of
the same film that DISPLAYS that frame -- which would make both this gate and
the intra-16x4 lift pixel-backed at once.

Suite (systemd unit, `cargo test -p ec-av1 --lib -j3`): `test result: ok. 425 passed; 0 failed; 38 ignored; 0 measured; 0 filtered out; finished in 730.11s`

## 5. EVIDENCE

EVIDENCE: ~/.cache/wit16x4-tmp/{o.raw,r.raw} | `decode_probe EC_PROBE_OUT16` on the pinned fixture vs `ffmpeg -pix_fmt yuv420p10le`, diff16.py 3840x1608 | 33 frames, all 0 differing bytes; rect64_corner_tu 148/123, intra16x4_in_inter 0/0/0
EVIDENCE: ~/.cache/wit16x4-tmp/census.txt | `decode_probe` on 2 film cuts + 4 1920x792 cuts under 6G scopes | only the 0 s cut decodes without a refusal, and it fires 0 intra 16x4/4x16 strips
EVIDENCE: ~/.cache/wit16x4-tmp/{h300t.obu,o3.raw,r3.raw} | 300 s cut truncated to its 8 decoded frame OBUs, decoded and compared | frames 1 and 2 differ by 14105680 / 12470780 bytes -> its 138/559/347 hits are phantoms
EVIDENCE: ~/.cache/wit16x4-tmp/{t6300.1.obu,o5.raw,r5.raw} + tcensus.txt | 6300 s cut truncated to 1 vs 2 frame OBUs | 1 OBU: exact, 0 hits; 2 OBUs: exact shown frame but 169/445/300 hits, all inside the no-show frame; 3/4/5 OBUs refuse
EVIDENCE: ~/.cache/wit16x4-tmp/ig.log | aomenc 1:4 gate `--include-ignored --nocapture` | FAILED, per-arm attempts [0, 0, 0], 0 pixel-exact attempts carrying the strip

## 6. Residue

- deferred(a witness): the intra 16x4/4x16-in-inter decode path is written and
  believed correct but unproven -- the unblockers are the 128x64-intra-on-inter
  refusal (a display frame would then follow the 6300 s cut's no-show frame
  that carries 169/445 strips) or the Golomb-tail refusal + whatever desyncs
  the 300 s cut's frame 1.
- accepted: `a_real_aomenc_inter_sequence_with_intra_16x4_strips_in_1to4_partitions_decodes_pixel_exact`
  stays `#[ignore]d`; its attempts 0/4 mismatch with NO 1:4 strip present, an
  open defect of another shape (lanes/intra16x4-r2.handoff.md).
