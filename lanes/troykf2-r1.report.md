# lane-troykf2 r1 — the "-ss 300 key frame is not pixel-exact" premise is STALE; LR stripe-0 / partial-bottom-stripe now gated

Base: `fedb7fe` (lane-sb128c r6 tip = main 85887c7 + 128-superblock support). Branch `lane-troykf2`.

## 1. The chartered defect does not reproduce (premise stale)

lane-interedge r1 reported the 1920x792 10-bit film's key frame at `-ss 300` wrong in rows 0..7
(max luma delta 12, 13710/15360 samples). Re-measured at this base, first thing, per COMMON:

    ffmpeg -ss 300 -t 2 -i <film> -c:v copy -an -f obu s300.obu     # bounded, under systemd-run --scope MemoryMax=6G
    python3 census4/trunc.py s300.obu 1 t300.obu                    # first temporal unit, 206172 bytes
    sha256 t300.obu (twice) = 9734f645854a9cc657040b7e6d7937a18aade34559610f62e7df4eb67bc3d993
    EC_PROBE_OUT16=o300.raw decode_probe t300.obu ; ffmpeg -i t300.obu -pix_fmt yuv420p10le -f rawvideo r300.raw

Result: **0 wrong samples in Y, U and V** (max delta 0), rows 0..7 included.

Whole key-frame table (`~/.cache/troykf2-tmp/kf.sh`, one truncated key frame per offset, ours
vs ffmpeg yuv420p10le, all three planes):

| -ss | stream sha (12) | result | wrong samples (all planes) |
|-----|-----------------|--------|-----|
| 0    | 9746d3df3bbf | OK | 0 |
| 300  | 9734f645854a | OK | 0 |
| 900  | b205be5ade93 | OK | 0 |
| 1800 | d514ad0d9397 | OK | 0 |
| 2700 | 16981094eaab | OK | 0 |
| 3600 | 66d309bcc22d | OK | 0 |
| 4500 | b40563ecd03f | OK | 0 |
| 5400 | c7a924ed7e86 | OK | 0 |
| 6300 | 3b9a4b380393 | OK | 0 |
| 7200 | 80e36a4cde45 | OK | 0 |
| 8100 | 23ce4869ccdb | REFUSED: 128x128 superblock HORZ/VERT or AB partition | n/a |
| 9000 | 26b410f82657 | REFUSED: 128x128 superblock HORZ/VERT or AB partition | n/a |
| 9900 | 748605ef91ac | OK | 0 |

11/11 decodable key frames pixel-exact; the 2 refusals are the residue lane-sb128c r6 already
named (128-root HORZ/VERT/AB), not this lane's.

EVIDENCE: ~/.cache/troykf2-tmp/kf.sh output above | extract -t 2 at 13 offsets, trunc to 1 frame, decode with EC_PROBE_OUT16, ffmpeg yuv420p10le reference | 11/11 decoded frames 0 wrong samples, 2 named refusals

## 2. What the round did instead: gate the stripe shapes nobody had gated

The charter's prime suspect (LR stripe 0, `RESTORATION_UNIT_OFFSET==8`) is correct code but was
untested: every LR gate size before this round (192x128, 160x96) is a whole multiple of 64 rows
and ran `--sb-size=64`, so no gate ever exercised the *partial* last stripe, and none asserted
that a real filter ran on stripe 0 at all (class `tool-disabled-in-every-gate`).

- `crates/ec-av1/src/restoration.rs:368` — new `LR_STRIPE0_HITS` / `LR_LAST_STRIPE_HITS`
  thread-locals + `lr_stripe0_hits()` / `lr_last_stripe_hits()`, bumped in
  `filter_restoration_unit` when a non-`None` filter runs on a stripe starting at plane row 0,
  resp. on a truncated stripe ending at `plane_h`.
- `crates/ec-av1/src/stream.rs:3395` — new gate
  `a_real_aomenc_10bit_partial_bottom_stripe_restoration_decodes_pixel_exact`: real aomenc,
  `--sb-size=128 --enable-restoration=1 --cpu-used=0`, 10-bit, three arms
  (256x152 cq15, 384x216 cq15, 256x152 cq25 — 152 = 2*64+24, 216 = 3*64+24, both leaving the same
  32-row tail stripe shape as the film's 792 rows), 16 seeds each, every decode-order frame
  compared against ffmpeg, `lr_hits` + `lr_stripe0_hits` + `lr_last_stripe_hits` hard-asserted
  per compared attempt by `ten_bit_tool_gate` (a refused attempt contributes no counts).
- `crates/ec-av1/src/stream.rs:13929` — the 8-bit LR gate's size moved 192x128 -> 192x152 (its
  stripe shapes at 128 are a strict subset) and it now hard-asserts the same two counters.

Results:

    a_real_aomenc_10bit_partial_bottom_stripe_..._exact 256x152 cq15: 16/16 attempts pixel-exact, lr_hits=33 lr_stripe0_hits=33 lr_last_stripe_hits=33
    a_real_aomenc_10bit_partial_bottom_stripe_..._exact 384x216 cq15: 16/16 attempts pixel-exact, lr_hits=54 lr_stripe0_hits=51 lr_last_stripe_hits=51
    a_real_aomenc_10bit_partial_bottom_stripe_..._exact 256x152 cq25: 16/16 attempts pixel-exact, lr_hits=24 lr_stripe0_hits=24 lr_last_stripe_hits=24
    a_real_aomenc_stream_with_restoration_reads_lr_symbols_correctly (8-bit, 192x152): 40/40 decoded and compared pixel-exact, wiener_hits=23 sgrproj_hits=78

EVIDENCE: cargo test -p ec-av1 --lib partial_bottom_stripe -- --nocapture | 48 real aomenc 10-bit sb128 streams over 3 sizes/cq, every shown frame vs ffmpeg | 48/48 pixel-exact, stripe0 108 / last-stripe 108 filtered stripes, 0 mismatches

No refusal was lifted (`refusal_inventory.rs` / `gate_coverage.rs` untouched — this round adds
coverage, it does not widen capability).

## 3. Open residue / dispositions

- deferred: a dedicated 8-bit arm of the *new* gate — accepted instead by moving the existing
  8-bit LR gate to 192x152 (same stripe shapes, 40/40 pixel-exact) — unblocked by a seeded 8-bit
  twin of `ten_bit_tool_gate`, which does not exist and would be a ~90-line duplicate.
- deferred: the 2 refusing film key frames (8100, 9000) — 128-root HORZ/VERT/AB partitions,
  lane-sb128c's named residue — unblocked by that lane.
- deviation: the branch is NOT rebased onto today's main (`cc323d0`); its base is the charter's
  `fedb7fe`, which is main 85887c7 + the 128-SB work the film needs. It merges into main after
  lane-sb128c.
