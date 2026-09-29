# lane-av1oraclerungdepth — make the oracle dump-rung DEPTH ASSUMPTION explicit

Branch: `lane-av1oraclerungdepth`, worktree `/home/tahinli/.cache/wt/av1rungdepth`,
base HEAD `aa0ac8c2` (main). **Comments only** — `scripts/instrument-aom-oracle.sh`,
+63 lines, every added line a `#` comment (verified: `git diff -U0 | grep '^+' |
grep -v '^ *#'` is empty). No decoder change, no gate change, no oracle rebuild,
no change to the shared `~/.cache/aom-oracle`.

## 1. What the four rungs are

Verified by reading the emitted C (no re-derivation):

| rung | env var | writes | depth check | paired decoder-side dump |
|---|---|---|---|---|
| 1 PREFILT | `EC_AV1_PREFILT_DUMP` | `fwrite(..., 1, y_crop_width, ...)` per row | **none** | `EC_AV1_PREFILT_DUMP`, `decode.rs:35263-35273` — `p.data.iter().map(\|&s\| s as u8)` |
| 6 POSTDEBLOCK | `EC_AV1_POSTDEBLOCK_DUMP` | `fwrite(..., 1, y_width, ...)` per row | **none** | `EC_AV1_POSTDEBLOCK_DUMP`, `decode.rs:35336` (dup `:53126`) |
| 7 PREFILT_WIDE | `EC_AV1_PREFILT_WIDE_DUMP` | `fwrite(..., 1, y_width, ...)` per row | **none** | `EC_AV1_PREFILT_WIDE_DUMP`, `decode.rs:19734-19762` (dup `:53048`) |
| 15 POSTCDEF | `EC_AV1_POSTCDEF_DUMP` | `fwrite(..., 1, y_width, ...)` per row | **none** | `EC_AV1_POSTCDEF_DUMP`, `decode.rs:35356` (dup `:53148`) |
| **12 FINAL** | `EC_AV1_FINAL_DUMP` | `fwrite(..., ec_hbd ? 2 : 1, ...)` | **yes** (`YV12_FLAG_HIGHBITDEPTH`) | `EC_AV1_FINAL_DUMP`, `stream.rs:2129-2155` (u16 LE) |

Each of the four is `fwrite(ptr, 1, w, f)` — one byte per sample, no
`YV12_FLAG_HIGHBITDEPTH` branch. Rung 12 is the only one that checks. All four
stay as-is: the pairing with a u8-narrowing decoder dump is the reason they
exist, and changing one side breaks the diff. **Behaviour unchanged.**

## 2. What was added

A `DEPTH ASSUMPTION` block directly under each of the four rung headers, at
the point a reader editing that rung reads first:

- `scripts/instrument-aom-oracle.sh:129` (rung 1), `:321` (rung 6), `:385`
  (rung 7), `:891` (rung 15)

Each block states, in six to ten lines: the assumption (1 B/sample, no
`YV12_FLAG_HIGHBITDEPTH` check), why it is deliberate (byte-paired with the
named decoder-side dump that narrows to u8, with its file:line), the failure
signature on an HBD stream (SIGSEGV / a file exactly half `w*h*bps`, which
reads like a truncated decode), and the HBD-correct alternative (rung 12
`EC_AV1_FINAL_DUMP`, with its line).

Two summary pointers were added so the trap is visible without opening a rung:

- `:10-17` — the file header, right under "Rungs provided", before the first
  rung entry.
- `:703-714` — the rung-12 block that already said "unlike the u8-narrowing
  debug dumps", now expanded into the full trap with the measured numbers and
  the `stat -c%s` vs `w*h*bps` check to run before blaming a decoder.

Final line references in the file (all verified against the committed
script): rung 1 block `:131`, rung 6 `:328`, rung 7 `:396`, rung 15 `:905`;
rung 1 emitted C `:146`, rung 6 `:352`, rung 7 `:416`, rung 15 `:934`,
rung 12 emitted C `:730`.

## 3. Item 2 — no assertion added, and why

The instruction's escape clause applies: **the script has no natural
non-byte-affecting failure point.**

- The script is a source patcher. It runs `python3 - "$F" <<'PY...'` sixteen
  times, rewriting `~/.cache/aom-oracle/src/av1/decoder/decodeframe.c`. It
  never runs `aomdec`, never opens a dump file, never has a frame index.
- Its only per-rung "ok" signal is 16 `print("... instrumented")` /
  `print("... already instrumented (no-op)")` lines emitted at
  *instrumentation* time. They fire before any byte is written.
- The script has no depth value. Grepped: no `bit_depth`, no `bps`, no
  `ftell`/`fseek`/`stat`/`getsize`, no run-time state at all.

A size assertion there would need the stream's `bit_depth`, the frame count and
the produced file path — none of which exist in this script. Plumbing them in
would mean the script parsing a fixture and re-running the oracle, which is
`build-aom-oracle.sh`'s job and is explicitly out of scope (no oracle
rebuild). **Comments alone shipped**, per the instruction.

The equivalent check a lane runs by hand is now written into the two summary
blocks: `stat -c%s <dump>.f0` against `w*h*bps`.

## 4. Proof

### 4a. Static

```
$ bash -n scripts/instrument-aom-oracle.sh
bash -n OK
$ # every <<'PY…' heredoc body compiled in isolation
python heredocs compile OK: 16
$ git diff -U0 scripts/instrument-aom-oracle.sh | grep -E '^\+[^+]' | grep -vE '^\+ *#'
(no output)          # every added line is a comment
```

### 4b. The trap, measured on the shared oracle (read-only use)

The shared build is already instrumented (`grep -c EC_INSTRUMENTED
decodeframe.c` = 7) and is a high-bitdepth build
(`aom_config.h: #define CONFIG_AV1_HIGHBITDEPTH 1`). It was only *run*, never
edited or rebuilt. Fixture: the committed
`crates/ec-av1/fixtures/av112bit-key.obu`, which the y4m header reports as
`W160 H128 C420p12` — 160·128 + 2·(80·64) = **30720 samples**, bps = 2, so
`w*h*bps` = **61440 B**.

Each env var tested in isolation:

| env var | exit | dump written |
|---|---|---|
| (none) | 0 | — (61496 B y4m) |
| `EC_AV1_FINAL_DUMP` (rung 12) | **0** | `fin.f0` = **61440 B** = `w*h*bps` ✅ |
| `EC_AV1_PREFILT_DUMP` (rung 1) | **139** | `pf.f0` = 0 B |
| `EC_AV1_POSTDEBLOCK_DUMP` (rung 6) | **139** | 0 B |
| `EC_AV1_PREFILT_WIDE_DUMP` (rung 7) | **139** | 0 B |
| `EC_AV1_POSTCDEF_DUMP` (rung 15) | **139** | 0 B |

The 139 is a genuine SIGSEGV inside this rung's own `fwrite`, not a decode
refusal — gdb on rung 1:

```
#3  fwrite ()
#4  av1_decode_tg_tiles_and_wrapup (pbi=…, …)      <- rung 1's insertion point
#5  read_one_tile_group_obu (…)
```

Same four rungs on the 8-bit fixture `444_lossy_superres_256x128_d12.obu`
(`W256 H128 C444`): rung 1 exits **0** and writes `p8.f0` = 65664 B. So the
four rungs are fine at 8 bits and broken at 12.

**Correction to the charter's premise, kept in the comments:** on this build
the four rungs do not reach the "silently half-length" state — they crash
first (0-byte file), which reads the same way, worse. Where the 1-byte write
*does* stay in bounds the arithmetic is exactly as predicted. Replicated on
the real rung-12 output rather than asserted:

```
$ python3 -c "…unpack fin.f0 as <30720H, take each low byte, write…"
rung12 EC_AV1_FINAL_DUMP size = 61440 (160*128 + 2*80*64 = 30720 samples x 2 B)
what a 1 B/sample rung emits   = 30720 == exactly half: True
expected 8-bit rung size (w*h*bps=1) = 30720
```

Both numbers are now in the comments: the comment at `:136-141` and the two
summary blocks name 61440 expected / 30720 actual, so a lane hitting this sees
the numbers it will actually measure.

### 4c. What could NOT be demonstrated without touching the shared oracle

- **The half-length file as an actual artifact.** On this build the four rungs
  SIGSEGV before finishing, so a real 30720 B file cannot be produced from the
  shared `aomdec`. Producing one would need a rebuild of an instrumented
  oracle with a fixed pointer/scale in the rung, which the charter forbids
  (no oracle rebuild) and which would also be a behaviour change. The 30720 B
  figure is therefore **derived byte arithmetic on a real 30720-sample file**
  (`fin.f0`), not a captured half-length dump.
- **The root cause of the SIGSEGV** is not diagnosed — only located to the
  rung's `fwrite` on the u16 buffer. Fixing it would change emitted bytes and
  break the pairing, i.e. out of scope here; it is a separate lane if anyone
  wants HBD pre-filter/post-deblock/post-CDEF ladders.
- **`bash scripts/instrument-aom-oracle.sh` was not executed end-to-end**, for
  the same reason: its first act is rewriting the shared oracle source. The
  `bash -n` plus 16/16 heredoc compile is the strongest check available
  without it.

## 5. Follow-ups this lane did not do (not done on purpose)

1. The four rungs could take a `YV12_FLAG_HIGHBITDEPTH` branch like rung 12,
   which would make them HBD-usable — but the decoder-side pair narrows to u8
   on purpose (`as u8`), so both sides would have to change together, and every
   existing byte-pairing gate would need re-pinning. That is a behaviour
   change, not a comment.
2. A reusable `stat -c%s … vs w*h*bps` check as a real assertion belongs in the
   gate that decodes through these rungs, not in the patcher.

## 6. Constraint compliance

- `scripts/instrument-aom-oracle.sh` only (plus this report).
- Comments only — no emitted byte changes (diff verified in §4a).
- No decoder change, no gate change.
- `~/.cache/aom-oracle` was executed read-only; no rebuild, no source edit.
  `git status --porcelain` in the primary checkout is unchanged (verified empty
  before and after).
- Never pushed.
