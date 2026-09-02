# lane-band63 r1 — the cq-63 "defect" does not exist; a real one at cq 32 does

Branch `lane-band63` off main `c729a38`, plus a merge of `lane-fi32x8` (c3a5032) so the
filter-intra-on configuration could be tested at all.

## 1. The chartered premise is FALSE (measured, not argued)

Charter: *"the 8-px band fixture at `--cq-level=63` mismatches ffmpeg on frame 0 for 8-bit
seeds 58/59/64 EVEN WITH `--enable-filter-intra=0`"* (from lanes/fi32x8-r1.report.md residue).

Reproduced the exact recipe (fi32x8 gate family: `geq=lum='mod(floor({axis}/8)*{step},256)'`,
`noise=alls={18+(attempt%4)*3}:all_seed={42+attempt}`, `--cq-level=63`, cpu-used=0,
1:4 partitions on, min/max partition 8/32, directional/paeth/smooth intra off) and swept it
far wider than the three named seeds:

| sweep | streams | result |
|---|---|---|
| fi32x8 family, attempts 0..23, 8+10 bit, `--enable-filter-intra=0` | 48 | 48 pixel-exact, 0 refusals |
| fi32x8 family, attempts 0..23, 8+10 bit, `--enable-filter-intra=1` (last) | 48 | 48 pixel-exact, 0 refusals |
| 32x32-gate family (noise 6..15), attempts 0..7, 8+10 bit, cq 63 | 16 | 16 pixel-exact |
| 32x32-gate family, seeds 58/59/64 x noise 6/9/12/15 x both axes, cq 63 | 24 | 24 pixel-exact |

Seeds 58/59/64 (= fi32x8 attempts 16/17/22) are inside the first two rows and are exact at
both depths. Streams hashed; generator `gen.sh`/`gen2.sh`/`gen3.sh` in the scratchpad.

EVIDENCE: $HOME/.cache/cargo-target-band63/debug/examples/decode_probe + ffmpeg rawvideo | 136 aomenc streams at cq 63 regenerated across 4 parameter sweeps, each decoded by ec-av1 and by ffmpeg and byte-compared full-frame (8-bit u8 dump, 10-bit u16 dump) | 136/136 identical, 0 refusals

Why the premise looked true in fi32x8's round: that lane's control ran `--enable-filter-intra=0`
*before* the base recipe's `--enable-filter-intra=1`, and **aomenc keeps the LAST occurrence**
(class aomenc-first-flag-wins, already in memory) — so the control flag never arrived, and what
it saw was its own then-unfixed filter-intra path, not "another shape". Class: stale-premise
lanes / refusal-names-a-correlate.

My own instrument repeated the class once: `decode_probe` had no 10-bit dump, so an
`EC_PROBE_OUT16` compare against a never-written file read as MISMATCH for three streams
(class stale-output-faked-measurement). Fixed by adding the dump, then re-measured.

## 2. What changed

- `crates/ec-av1/examples/decode_probe.rs:81` — `EC_PROBE_OUT16` dumps planes as
  little-endian u16, the only form a 10-bit pixel diff against
  `ffmpeg -pix_fmt yuv420p10le` can use. Without it no 10-bit claim from this instrument is real.
- `crates/ec-av1/src/stream.rs:16757` — the 32x32-level 1:4 gate's second quantiser is **63**
  instead of 45 (attempts 2,3,6,7 at both depths). The cq-32 attempts keep their exact streams,
  so nothing changes by accident. cq 63 is now coded, so the premise cannot come back unmeasured.
- `crates/ec-av1/src/stream.rs:16992` — the filter-intra 1:4 gate's rotation is
  `[32,45,55,63]` (attempts 6,7,14,15 at cq 63), same reason, with filter intra ON.

Both gates keep every existing hard assert (per-depth `horz_proved > 0 && vert_proved > 0`,
`coeff_proved > 0`, `matched > 0`, `out_of_scope_mismatch == 0`, full-frame Y/U/V compare).

Gate run:
```
EC_NOMEMGUARD=1 EC_AV1_REQUIRE_AOMENC=1 CARGO_TARGET_DIR=$HOME/.cache/cargo-target-band63 \
  cargo test -p ec-av1 --lib -j3 -- --nocapture 1to4
```
→ `test result: ok. 3 passed; 0 failed` (14.7 s):
- 32x32-level 1:4: 8-bit 8/8 exact, horz_4=56 vert_4=84 coded=140; 10-bit 8/8 exact,
  horz_4=56 vert_4=68 coded=124.
- filter-intra 1:4: 8-bit 5 matches/3 refusals, 54x(32x8)+5x(8x32); 10-bit 6 matches/1 refusal,
  9x(32x8)+17x(8x32).
- superblock-level 1:4 (sibling, untouched): ok.

EVIDENCE: gate stdout above | both 1:4 gates run with cq 63 in the rotation at 8 and 10 bit | 3 passed / 0 failed, cq-63 attempts pixel-exact with the 1:4 counters non-zero per depth

## 3. A REAL defect found on the way (NOT the chartered one)

The first rotation I tried (`[32,45,63]`) also moved attempt 6 from cq 45 to cq 32, creating a
stream nothing had ever coded — and it mismatches. Fully bisected, then set aside because it is
not this lane's charter and not reachable from the shipped rotation:

- Stream: 192x128, 8-bit, seed 48, `geq` axis Y, step 103, `noise=alls=12:all_seed=48`,
  **`--cq-level=32`**, otherwise the 32x32-level 1:4 gate recipe (filter intra off).
  Pinned at `/tmp/.../scratchpad/b63/gateA.obu` (regenerate: `gen.sh 48 Y 103 12 32 8 <out>`).
- Stage: **reconstruction**, not filtering — `EC_AV1_PREFILT_DUMP` already differs, identically
  to `EC_AV1_POSTDEBLOCK_DUMP` (9057 luma samples, first at x=64,y=64).
- First divergent element (range ladder, `EC_TRACE_MODE`/`EC_TRACE_MODE_STEP`, ours vs the
  instrumented aomdec, aligned by (mi_row, mi_col, name)): **`mi_row=24 mi_col=8`, element
  `mode`** — identical entry range 46952 on both sides, then aomdec `val=12 rng=36608`,
  ec-av1 `val=7 rng=33744`. Same range in, different value and range out = **wrong y-mode CDF
  row**, i.e. a wrong intra-mode neighbour context, not a wrong table value.
- Context: the block is `BLOCK_32X16` (aomdec `bsize=8`) at the 32x32 level; its *above*
  neighbour is the last 32x8 `PARTITION_HORZ_4` strip at (22,8) (mode 12/PAETH) and its *left*
  is the 32x8 strip at (24,0) (mode 0/DC). `decode_block_rect4` (decode.rs:5990) stamps the
  COARSE 16-px `above_mode`/`left_mode` cells with "last strip wins"; the suspect is that
  coarse stamp seen by a 32x16 sibling in the next quadrant (class
  context-read-from-one-cell / neighbour-votes-all-its-fields).
- Everything before that element is bit-exact: the ladder matches element for element through
  `mi_row=22`, so the entropy stream, the strips' own coefficients and their pixels are right.

EVIDENCE: /tmp/.../scratchpad/b63/{gateA.obu,ours.trace,aom.trace,o.pre.f0,a.pre.f0} | ec-av1 vs instrumented aomdec range ladders + pre-filter/post-deblock dumps on the pinned stream | first divergence mi_row=24 mi_col=8 `mode` (entry rng 46952 both, out 36608 vs 33744); recon differs from (64,64), 9057 luma samples, deblock-only bleed at rows 62-63

## 4. Residue

- deferred: the seed-48 @ cq-32 y-mode-context defect above — a wrong CDF row for a 32x16 block
  whose above neighbour is a 32x8 1:4 strip — what unblocks it: reading the above/left mode ctx
  ec-av1 computes at (24,8) against libaom's `intra_mode_context[]` of the same two neighbours
  (the ladder, stream and stage are already pinned above, so it starts at the fix, not the hunt).
  Not reachable from the shipped gate rotation (attempts 0,1,4,5 keep their original cq-32
  streams and pass).
- accepted: this lane merges `lane-fi32x8` (c3a5032, merge-ready per its own report) because the
  filter-intra-on arm of the premise cannot be tested on main, which still refuses those strips.
  If fi32x8 lands on main first, this merge is a no-op.
- accepted: no refusal is lifted this round — nothing was refused. The deliverable is a coded
  quantiser the gates never coded, and a disproof.

## 5. Suite

```
systemd-run --user --unit=band63-suite-... -p MemoryMax=10G --same-dir bash -lc \
  'EC_NOMEMGUARD=1 EC_AV1_REQUIRE_AOMENC=1 CARGO_TARGET_DIR=$HOME/.cache/cargo-target-band63 \
   nice -n 10 cargo test -p ec-av1 --lib -j3 > $HOME/.cache/band63-suite.log 2>&1'
```
→ `test result: ok. 340 passed; 0 failed; 31 ignored` (337.9 s).

`a_real_aomenc_multi_tile_intra_stream_decodes_pixel_exact` — red on main 11633d7 when
lane-fi32x8 reported — is green here: lane-mtfix fixed it in main c729a38, this base carries it.

EVIDENCE: $HOME/.cache/band63-suite.log | full `cargo test -p ec-av1 --lib` on lane-band63 c099bef with the oracle required | 340 passed / 0 failed / 31 ignored
