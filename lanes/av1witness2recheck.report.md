# lane-av1-witness2recheck: W1-7 — do the two witness2 4:4:4 panics still fire on current main?

Measurement lane. No decoder edit, no fixture committed. Tree: lane/av1witness2recheck
at `b8385c93` (main, worktree `/home/tahinli/.cache/wt/av1witness2recheck`).

**Verdict: 0 / 2 still panic.** Both recipes reproduce the *hunt's exact bytes*
(sha256 identical to the `fe3e8418` report) and both decode clean AND byte-exact
against the aomdec oracle on current main. No Wave-1 lane exists for either site;
both are CLOSED, and both were closed by named landed lanes, bisected here.

## Provenance of the measuring binary

`CARGO_TARGET_DIR=/home/tahinli/.cache/tgt/av1witness2recheck cargo build -p ec-av1
--example decode_probe`, binary sha256
`23f9083717e1d75f6ae29b18d0cef23ae7af054fa6ce426e0d1fb7d9316494bf`. Provenance
check: `strings -a` over the binary yields exactly one tree path,
`/home/tahinli/.cache/wt/av1witness2recheck` — no sibling lane or primary-checkout
path appears in its embedded strings. The control binary (below) built at
`fe3e8418` in `/home/tahinli/.cache/wt/av1w2r-old` embeds only its own tree path.

## Recipes — bytes reproduced, no drift

Encoder `$HOME/.cache/aom-oracle/build/aomenc`, ffmpeg 8.1.2 lavfi, exactly the
hunt's command lines (`lanes/av1witness2.report.md` Annex A).

| # | stream | hunt sha256 | mine sha256 | bytes |
|---|---|---|---|---|
| P1 | `s1d.obu` (bands444 + sb128 128-root rect) | `68773f2c6e01a2d…1925b1` | `68773f2c6e01a2d211bf0213479accb4c2c360132c0e1bc3b326eab7861925b1` | 595 |
| P2 | `s1e.obu` (testsrc2 444 + sb64 rect/1:4) | `72bed2791d1ceba8…d3593af` | `72bed2791d1ceba8be7926a052c7838c5d96d4f9d56d28fb69789a753d3593af` | 3577 |

Both hashes match the hunt's byte for byte: **the recipes did not drift**, so these
are the same witnesses, not lookalikes. Input y4ms hashed too
(`bands444.y4m` `fa6218399cce8dd…`, `busy444.y4m` `53115212caaefda4…`).

## Outcome on current main

Both: `OK: 3 frames decoded, 320x256`, rc=0, empty stderr, no refusal string.
Oracle comparison (three independent runs of our decode are byte-identical to each
other — no nondeterminism):

| stream | ours vs `aomdec --rawvideo` | ours vs `ffmpeg -pix_fmt yuv444p -f rawvideo` |
|---|---|---|
| s1d.obu | **identical** (737280 B) | **identical** |
| s1e.obu | **identical** (737280 B) | **identical** |

(aomdec and ffmpeg outputs are themselves byte-identical on both streams, so the
two oracles agree.)

Census on main (proves the arms are reached, not that the run was vacuous):
`s1d`: `sb128_rect: edge_vert=6`, `part128: vert=6 intra_vert=6`,
`rect4_32: vert=8 coded=8` — the 128-root VERT strip path the old assert guarded
fires 6 times. `s1e`: `leaf8_intrabc_hits: 7`, `cfl_ac: 444=72`,
`rect_intrabc_vartx: 6`, `inter_rect: 32x8=4`.

## Control: the hunt's panics are real at `fe3e8418`

Same two byte-identical streams, binary built from a detached `fe3e8418` worktree:

- s1d.obu → rc=101, `panicked at crates/ec-av1/src/decode.rs:17631:5:
  assertion failed: lossless_frame || (chroma_w / chroma_tx, chroma_h / chroma_tx)
  == (nch_x, nch_y)` — the hunt's P1, at the hunt's line.
- s1e.obu → rc=101, `panicked at crates/ec-av1/src/decode.rs:3766:16: index out of
  bounds: the len is 16 but the index is 16`, backtrace
  `PlaneBuf::reconstruct` (16305) ← `exec_intra` (3766) ← `inline_intra` (3379) ←
  `push_intra` (3426) ← `decode_leaf8` — the hunt's P2, at the hunt's line.

So the streams are still live witnesses of the old defects; only main has moved.

## What closed each

`git bisect` over `b8385c93 … fe3e8418` in the old worktree, verdict = panic
reproduces (bisect `good`) vs not (`bad`):

**P1 — closed by `349b3918` (lane-av1-444sb).** First commit where the
`decode_block_128rect` chroma-tiling assert no longer fires is `349b3918`, whose
own message states the cause: the 128-rect chroma unit walk was hardcoded to the
4:2:0 mu-chunk span, so at 4:4:4 a 128-root HORZ/VERT block coded half its chroma
units. The site is now the per-axis invariant at `decode.rs:24515`:
`(chroma_w/chroma_tx, chroma_h/chroma_tx) == (nch_x*((64>>ss_x)/chroma_tx),
nch_y*((64>>ss_y)/chroma_tx))`. Bisect trace (s1d): panic at `b3f815c8` and
`76f8c3fe`, clean from `349b3918` on.

**P2 — closed by `b6d4b528` (lane-av1leaf8oob).** First commit where the OOB no
longer fires. Its report names the one site: `decode_leaf8`'s intrabc frame-copy
closure sized the leaf chroma buffers with the literal 4:2:0 halving
(`vec![0u16; 4*4]`) while at 4:4:4 the leaf's chroma block is 8x8, so the 16-sample
override reached `reconstruct` as an 8x8 prediction and indexed `prediction[16]`.
Its gate `an_intrabc_8x8_leaf_chroma_frame_copy_at_444_decodes_without_the_oob`
pins these exact bytes as `crates/ec-av1/fixtures/444_leaf8_oob.obu` (sha256
`72bed279…d3593af`, byte-identical to mine) and passes on this tree
(`cargo test -p ec-av1 --lib an_intrabc_8x8_leaf_chroma_frame_copy_at_444_decodes_without_the_oob`
→ 1 passed, 0 failed).

**Byte-exactness is a later, separate fix — `fa70a68c` for BOTH streams.**
`b6d4b528` removed the P2 panic but its own report deferred the pixel divergence
("281306 / 737280 wrong"). Bisecting the *oracle comparison* (not the panic) shows
both s1d and s1e are byte-exact from `fa70a68c`
("4:4:4 intrabc rect chroma: size the plane block from ss_size_lookup, not a
halving") and diverging at its parent `f33b9d41` — measured directly, both
streams: `fa70a68c` EXACT, `f33b9d41` not exact (213706 B for s1d, 281306 B for
s1e). So the two defects the hunt recorded are two defects plus one shared
pixel-exactness fix, all landed.

## Per-recipe verdict

| recipe | on `fe3e8418` | on `b8385c93` (main) | verdict |
|---|---|---|---|
| P1 `s1d.obu` `68773f2c…` | panic, `decode.rs:17631` 128-rect chroma assert | clean 3/3, **byte-exact** vs aomdec + ffmpeg | **CLOSED** by `349b3918`; exactness by `fa70a68c` |
| P2 `s1e.obu` `72bed279…` | panic, `decode.rs:3766` OOB in `PlaneBuf::reconstruct` | clean 3/3, **byte-exact** vs aomdec + ffmpeg | **CLOSED** by `b6d4b528`; exactness by `fa70a68c` |

No Wave-1 lane to charter for either site. No fixture bytes needed for a Wave-1
pin: P2's bytes are already pinned in-tree, P1's (`68773f2c…`) are **not pinned
anywhere** — the only 4:4:4 sb128 fixture in the tree is
`444_sb128rect_lr_witness.obu` (`27825e14…`, 794 B), a different stream from
`s1d.obu`. The P1 bytes live only at `/tmp/av1w2r/s1d.obu` plus the recipe above;
if anyone wants a permanent pin for the 128-rect 4:4:4 shape, this is the stream,
and it is now byte-exact so it would make a good regression fixture.

## Commands

```
# recipes (hunt's exact lines; ffmpeg 8.1.2 -> $HOME/.cache/aom-oracle/build/aomenc)
ffmpeg -f lavfi -i "nullsrc=s=320x256:rate=30:d=1,geq=lum='mod(floor((Y+T*160)/8)*67,255)':cb='128+20*sin(Y/17+T)':cr='128+20*sin(X/23+T*2)'" -pix_fmt yuv444p bands444.y4m
aomenc --codec=av1 --obu -o s1d.obu --passes=1 --threads=1 bands444.y4m --limit=3 --kf-max-dist=1 --cpu-used=2 --end-usage=q --cq-level=20 --sb-size=128 --enable-rect-partitions=1 --enable-1to4-partitions=1 --min-partition-size=4
ffmpeg -f lavfi -i testsrc2=size=320x256:rate=30:duration=1 -pix_fmt yuv444p busy444.y4m
aomenc --codec=av1 --obu -o s1e.obu --passes=1 --threads=1 busy444.y4m --limit=3 --cpu-used=0 --end-usage=q --cq-level=45 --sb-size=64 --enable-rect-partitions=1 --enable-1to4-partitions=1 --min-partition-size=4

# current main
CARGO_TARGET_DIR=$HOME/.cache/tgt/av1witness2recheck cargo build -p ec-av1 --example decode_probe
~/.cache/tgt/av1witness2recheck/debug/examples/decode_probe s1d.obu      # OK: 3 frames decoded, 320x256
~/.cache/tgt/av1witness2recheck/debug/examples/decode_probe s1e.obu      # OK: 3 frames decoded, 320x256
EC_NOMEMGUARD=1 EC_PROBE_OUT=ours.raw ~/.cache/tgt/av1witness2recheck/debug/examples/decode_probe s1d.obu
~/.cache/aom-oracle/build/aomdec --rawvideo -o oracle.raw s1d.obu ; cmp ours.raw oracle.raw   # identical
ffmpeg -i s1d.obu -pix_fmt yuv444p -f rawvideo ff.raw -y ; cmp ours.raw ff.raw               # identical

# control at fe3e8418 (detached worktree /home/tahinli/.cache/wt/av1w2r-old)
CARGO_TARGET_DIR=$HOME/.cache/tgt/av1w2r-old cargo build -p ec-av1 --example decode_probe
~/.cache/tgt/av1w2r-old/debug/examples/decode_probe s1d.obu   # rc=101, decode.rs:17631 assert
~/.cache/tgt/av1w2r-old/debug/examples/decode_probe s1e.obu   # rc=101, decode.rs:3766 OOB
```

## Instrument limits

- The two bisects are over commits in this repo only; they say which landed commit
  flips the measured predicate, not that no other commit also touches the site.
- The oracle comparison uses one stream per predicate (s1d for P1, s1e for P2), so
  `fa70a68c` is "the first commit at which *these two streams* are byte-exact", not
  a claim about the whole 4:4:4 class.
- Local scoped runs only (two example builds, one named gate test). No full suite
  was run on this lane.
