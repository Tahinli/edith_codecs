# lane-intertx — inter tx-type search on film (TX_TYPE_SEARCH_INTER past screen-only)

GOAL: the inter tx-type search (`inter_tx_type_search`, encode.rs ~L450)
runs only on screen content at the preset lever
(`crate::speed::TX_TYPE_SEARCH_INTER` → `InterTxSearch::Screen`).
`EC_AV1_TXSET_INTER=all` exists but has never been measured on film, and
the `inter_luma_set` doc (encode.rs ~L475) records a deferred CHROMA
inheritance desync: (a) `FrameCtx::reduced_tx_set_inter` never set while
`recode_inter_chroma` reduces against the reduced allowance, and (b)
`tile::write_block_planes` codes every chroma plane `DCT_DCT`, so an
inherited 1-D luma type desyncs chroma. This lane makes inter tx-type
search correct and measured on film, then ships or parks by the keep
rule.

Work ONLY in this worktree (`lane-intertx`,
`/home/tahinli/Documents/Code/Rust/edith_codecs-intertx`). Absolute paths
only. Never touch main's checkout or the rectres worktree, never push,
never `git checkout <file>`, never repo-wide `cargo fmt` (rustfmt only
files you edited). Local builds: `export
CARGO_TARGET_DIR=$HOME/.cache/cargo-target-intertx`. Local logs under
`~/.cache/intertx/`. Media files read-only. Pins 8291 / 33227. Keep
rule: one film row ≥0.5 BD down on a column, the other flat within
±0.3, screen not worse by 0.3, wall ≤+15%. BD-rate, lower better.

## 1. Correctness first (witnesses BEFORE any BD gate)

- Determine whether the chroma inheritance desync is LIVE when the
  search offers non-`DCT_DCT` types on inter blocks: build a witness in
  the standing pattern (process-global `set_inter_tx_search` override +
  `crate::speed::knob_write`, like the existing inter-tx witness) on
  NON-screen content where the search picks a 1-D type, decoded
  sample-exact through ffmpeg AND `decode_stream` (all planes — the
  desync is chroma). If it desyncs: fix (a) and (b) — the decoder's
  `decode::reduce_inherited_chroma_tx_type` is the spec behavior; the
  encoder must set `reduced_tx_set_inter` honestly and code chroma with
  the inherited reduced type, not unconditional `DCT_DCT`.
- Witness must assert the search actually WON a non-`DCT_DCT` inter
  type (a `coded > 0`-style counter guard — class
  `gate-blind-to-feature`), on a frame the detector calls non-screen.

## 2. Measure (control = Screen, arm = All)

Both on the SAME VPS (see §3), same HEAD:

- `encode::tests::bd_rate_film_long_gop` (deciding, both films):
  control = default; arm = `EC_AV1_TXSET_INTER=all`.
- `encode::tests::bd_rate_screen_native` (all five rows), control and
  arm. The screen row must not regress (arm should equal control there
  — screen already searches; if it moves, find out why).
- Census: non-`DCT_DCT` inter tx-type picks per clip (search won /
  coded), printed per run; report per-frame rates like lane-b128res §4.

Keep rule met → make the lever `All` at the presets where it measured
(a speed.rs preset-table change), pins at default and `EC_AV1_SPEED=6`,
three-lane lib suite LOCALLY (`--skip stream::` / `stream:: --skip
10bit` / `10bit`), `timeout 900 cargo check --workspace --all-targets
-j4` (0 ec-av1 warnings). Not met → leave `Screen`, document.

## 3. Gates run on VPS-1 (the box stays free for lane-rectres)

VPS-1 = `tCloud@2.28.112.3` (Fedora 44, 4 vCPU, 7G RAM, rust 1.98.1,
ffmpeg with libaom-av1+librav1e). Provisioned layout:
`/home/tCloud/gates/library/fixtures/` (bars fixtures + rewritten
real-library-manifest.tsv), `/home/tCloud/gates/library/Films/
filmA-gate.mkv` + `filmB-gate.mkv` (windowed substitutes, sha256-verified
pixel-identical to the originals at the pinned seeks — 48 frames each),
`/home/tCloud/gates/library/OBS/…mkv` (screen row),
`/home/tCloud/gates/bin/ffmpeg` (shim that rewrites the pinned `-ss`
values onto the windows; all other invocations exec `/usr/bin/ffmpeg`
untouched).

Gate recipe per run (control and arm alike):

```
rsync -a --delete --exclude /fixtures/ --exclude /target/ \
  --exclude /.git/ \
  /home/tahinli/Documents/Code/Rust/edith_codecs-intertx/ \
  tCloud@2.28.112.3:/home/tCloud/gates/repo-intertx/
ssh -o BatchMode=yes tCloud@2.28.112.3 'ln -sfn \
  /home/tCloud/gates/library/fixtures \
  /home/tCloud/gates/repo-intertx/fixtures'
ssh -o BatchMode=yes tCloud@2.28.112.3 'cat /proc/loadavg; cd
  /home/tCloud/gates/repo-intertx && PATH=/home/tCloud/gates/bin:$PATH \
  CARGO_TARGET_DIR=/home/tCloud/gates/target-intertx \
  [EC_AV1_TXSET_INTER=all] \
  systemd-run --user -p MemoryMax=5G --wait --unit=intertx-<name> \
  --pipe cargo test -p ec-av1 --release --lib -- --ignored --exact \
  --nocapture encode::tests::bd_rate_film_long_gop; cat /proc/loadavg' \
  | tee ~/.cache/intertx/<name>.log
```

If `systemd-run --user` is unavailable on the VPS (no user session
bus), run bare and note it. First run compiles (~15 min on 4 vCPU) —
that is normal. ONE gate at a time on the VPS. LOADAVG before AND
after, recorded. `--ignored --exact` is load-bearing (the tests are
`#[ignore]`). TMPDIR never on any cargo line.

## DONE

Commits on `lane-intertx` + `lanes/intertx.report.md` with EVIDENCE
lines (`EVIDENCE: <artifact> | <steps> | <measurement>`), witness
results, BD tables control vs arm on both gates, tx-type census, wall
deltas, LOADAVG pairs, pins line, disposition `ships-on | stays-off |
stopped`. Never merge or push.
