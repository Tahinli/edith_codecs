# lane-cdefstrip r1 — report (GREEN gate, charter premise DISPROVED, no fix needed)

Branch `lane-cdefstrip`, rebased onto main `3566501` (charter said "main >= dfe9ce8";
main had moved two merges further, so the premise was re-measured on the current tree).

## 1. The charter's defect no longer exists
Charter premise: "a real aomenc 8-bit stream is +-1 wrong on 4 luma px of decode-order
frame 1 (cols 48/54/55/59) inside a SKIPPED 32x8 inter strip's row" (lane-inter16ab r6,
`~/.cache/inter16ab-tmp/a8.sh`).

Re-measured on main 3566501 (which contains lane-cdef 1176a16): that stream is now
pixel-exact on frames 0-4 including the named pixels. The +-1 band was the 8x8-leaf
skip-band gap lane-cdef fixed; the charter was written from the pre-merge measurement.

EVIDENCE: ~/.cache/cdefstrip-tmp/{a8.obu,ours.yuv,ref.yuv} | `bash a8.sh` (sha256
`6d8b29d903ef5f8deb8e0a6b79798b33291373be9ffc50f750ce3cbc6f38170a`, hashed twice, identical);
`EC_PROBE_OUT=ours.yuv decode_probe a8.obu` vs `ffmpeg -i a8.obu -f rawvideo` |
frames 0,1,2,3,4 luma diff = 0 px each (was 4 px on frame 1); frame 5 differs — see 4.

## 2. "Rect strip arms never write the CDEF skip band" is FALSE — measured, not argued
Built the direct instrument instead of auditing arms one by one: `Neighbours::skip_written`
(decode.rs:4046, set in `fill_skip_grid_rect` decode.rs:4536-4541) records whether ANY coded
block wrote the per-mi skip band this frame; `apply_cdef` (decode.rs:13363-13405, a pass placed
BEFORE both early returns so it is valid for every frame) counts 8x8 CDEF units with an
unwritten band into `decode::cdef_unwritten_skip_units()`. A fresh `Neighbours` seeds
`skip_grid` all-`false`, so an arm that forgets the write is exactly an unwritten unit —
the defect class, observable without pixels.

Result: **0 unwritten units on 177 real aomenc streams** — 145 pre-existing corpus streams
(`~/.cache/inter16ab-tmp/*.obu` + `crates/ec-av1/fixtures/*.obu`, incl. 32x8/8x32/64x32/32x64/
64x16/16x64 strips, AB and 1:4 arms, 8- and 10-bit) plus 32 new sweep streams
(`~/.cache/cdefstrip-tmp/sw.sh`: 2 sources x cq 18/24/32/45 x min-partition-size 4/8 x 8+10 bit).

EVIDENCE: ~/.cache/cdefstrip-tmp/sw.sh + corpus sweep | `decode_probe <stream>` per stream,
grep `cdef_unwritten_skip_units` | streams=145 with_unwritten=0, plus 32/32 sweep streams at 0;
a8.obu shows `rect_skip_writes=16` (skipped rect strips that DID write the band).

Also checked by reading, not assumed: `is_8x8_block_skip`'s all-four-cells rule is already
`Neighbours::is_skip_txfm` (decode.rs, gathered over the 2x2 mi span, matching cdef.c:29-38),
and `maybe_read_cdef_idx` (decode.rs:412-416) returns before consuming the literal when the
block is `skip`, so a skipped 32x8 strip does not eat the 64x64's `cdef_idx` — the charter's
second suspicion.

## 3. New gate (regression fence for the class)
`stream.rs: a_real_aomenc_stream_with_skipped_rect_inter_strips_keeps_the_cdef_skip_band`
— real aomenc, `--enable-cdef=1 --enable-rect-partitions=1 --enable-1to4-partitions=1
--min-partition-size=8 --sb-size=64`, 6 arms (8+10 bit x cq 18/24/45 x two motion sources),
every decode-order frame compared on all three planes (stricter than out_of_scope_mismatch==0),
hard asserts: `cdef_unwritten_skip_units()` unchanged (==0 growth) per compared attempt;
per-attempt firing requires a skipped rect strip to have written the band AND an 8x8 unit to
have been excluded by the all-skip rule; >=3 firing+exact attempts.

EVIDENCE: cargo test output | `EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib
skipped_rect_inter_strips -- --nocapture` | ok, "4 firing+exact attempts, unwritten skip-band
units 0, skipped rect strips 49, 8x8 units excluded 1630"; 2 non-firing arms recorded
(one SB-level AB refusal, one 10-bit arm where aomenc picked no skipped strip).

## 4. Residue found, NOT this lane's class
The LAST decode-order frame (frame 5) of 3 of the 32 sweep arms mismatches
(8-bit only: source-a cq18 945 px max|d|=42; source-b cq24 min-part 4 and 8, 8370 px
max|d|=87; a8.obu cq22 1349 px). It is NOT CDEF: re-encoding the identical source with
`--enable-cdef=0` (`a8n.sh`) still mismatches that frame (5479 px), while frames 0-4 are exact
in both. Every 10-bit twin of those arms is exact on all 6 frames.
Disposition: deferred(needs its own lane: last-frame-only 8-bit inter defect, prediction or
residual, unblocked by bisecting b10/a8 frame 5 with the pre-filter dump) — it is why the gate's
arm list is the measured-exact subset rather than the whole sweep.

Second residue: `cdef_mixed_skip_units()` (an 8x8 CDEF unit whose four mi cells disagree on
skip — the HORZ_4 shape the charter names) is 0 on all 177 streams, because the only blocks
that can straddle an 8x8 unit are 16x4/4x16 and sub-8x8 inter, both still refused
("an inter 16x16-level 1:4 partition", "an inter partition below 8x8").
Disposition: accepted — unreachable until those refusals lift; the counter is in place so the
day one lifts, this gate's arms report it.

## 5. Refusals
None lifted (none of this lane's class exists). `refusal_inventory` / `gate_coverage` untouched
and green.

## 6. Suite
`cargo test -p ec-av1 --lib` under a systemd unit -> $HOME/.cache/cdefstrip-suite-r1.log: see
the line appended below.

`cargo test -p ec-av1 --lib` (unit `cdefstrip-suite-1788349014.service`, log
$HOME/.cache/cdefstrip-suite-r1.log): **401 passed, 0 failed, 33 ignored**, 1396.72s.
