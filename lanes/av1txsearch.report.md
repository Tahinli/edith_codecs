# lane-av1txsearch — settle the `enable-tx-size-search` census floor

Worktree `~/.cache/wt/av1txsearch`, branch `lane-av1txsearch`, based on `1aaf7e0b`
(lane-av1distwtd r3, the call-resolving census fix — read as prior art, not
duplicated). Owns `crates/ec-av1/src/gate_coverage.rs` only. `stream.rs` and
every decoder file are byte-identical to the base.

## The claim that was a floor

r3 left this in-file, above `DEFAULT_ON_TOOLS`:

> 113 of the 230 selected gates name it: 88 pass `=0`, 15 pass `=1`, and 9
> build the value into a `format!`/`String` variable that neither `flags_in`
> nor `settings_in` can see … Until that exists the count above is a floor, not
> a measurement: 9 gates are unclassified in an unknown direction.

An unknown direction is the failure this file exists to prevent, so the entry
could not be retired on the numbers as they stood. This lane settles it.

## 1. The templates

`grep -n 'format!("--enable-tx-size-search' crates/ec-av1/src/stream.rs` returns
**10** sites, not 9 — the extra one is a segment that spells the flag BOTH as a
literal and through a template, so r3's literal reader filed it under the
literal and it never showed up in the unknown column. All ten are `format!` with
an inline captured placeholder; there is no `String::from`/`to_string`/
`push_str` shape and no positional-`{}` shape for this flag. The single
positional-`{}` site in the file is a different flag
(`a_real_aomenc_stream_with_ab_partitions_below_16x16_decodes_pixel_exact` uses
`u8::from(a.tx_size_search)`, below).

Four binding shapes cover all ten:

| # | template | binding shapes seen |
|---|----------|--------------------|
| T1 | `format!("--enable-tx-size-search={txs}")` | `for txs in [0usize, 1]`, `for txs in ["0", "1"]` |
| T2 | `format!("--enable-tx-size-search={tx_search}")` | `let tx_search = if attempt % 2 == 0 { "0" } else { "1" };` and the `attempt == 0` variant |
| T3 | `format!("--enable-tx-size-search={tx_search}")` | `for (tx_search, bit_depth) in [("1", 8u8), ("0", 8u8), ("1", 10u8)]` (tuple-destructured loop) |
| T4 | `format!("--enable-tx-size-search={}", u8::from(a.tx_size_search))` | positional arg; value from a struct-literal arm table's `tx_size_search: true/false` rows |
| T5 | `for (tag, cq, w, h, ten_bit, vert, tx_search) in arms` + `args.push(if tx_search { "…=1" } else { "…=0" })` | **not** a template — both literals are spelled, so the literal reader saw it. Listed because it is the same tool and was in the "both values" column. |

## 2. The binding table

`gate` = the census `#[`-segment name, `use site` = the template's shape, `bound
value` = what the resolver reads from the gate's own text, `evidence` = the
literal source line the binding came from.

| gate | use site | bound value | evidence |
|------|----------|-------------|----------|
| `a_real_aomenc_palette_stream_with_8x8_leaves_decodes_pixel_exact` | T1 | `0` and `1` | `stream.rs:9437` `for txs in [0usize, 1] {` |
| `a_coded_rect_intrabc_block_reconstructs_in_both_orientations` | T1 | `0` and `1` | `stream.rs:10901` `for txs in ["0", "1"] {` — this is the segment that ALSO spells the flag literally, which is why r3 counted 9 not 10 |
| `a_real_aomenc_inter_sequence_with_temporal_mv_candidates_decodes_pixel_exact` | T2 | `0` and `1` | `stream.rs:19665` `let tx_search = if attempt % 2 == 0 { "0" } else { "1" };` |
| `a_real_aomenc_inter_sequence_with_a_coded_rectangular_residual_decodes_pixel_exact` | T2 | `0` and `1` | same line shape, own segment |
| `a_real_aomenc_inter_sequence_with_a_16_level_rect_leaf_decodes_pixel_exact` | T2 | `0` and `1` | same line shape, own segment |
| `a_real_aomenc_inter_sequence_with_16x16_level_ab_partitions_decodes_pixel_exact` | T2 | `0` and `1` | same line shape, own segment |
| `a_real_aomenc_inter_sequence_with_16x16_level_1to4_partitions_decodes_pixel_exact` | T2 | `0` and `1` | same line shape, own segment |
| `a_real_aomenc_stream_with_ab_partitions_below_16x16_decodes_pixel_exact` | T4 | `0` and `1` | `stream.rs:26084`+ `tx_size_search: false` rows and `:26150`+ `tx_size_search: true` rows in the arm table |
| `a_real_aomenc_stream_with_filter_intra_on_a_sub8_rect_leaf_decodes_pixel_exact` | T3 | `0` and `1` | `stream.rs:31131` `for (tx_search, bit_depth) in [("1", 8u8), ("0", 8u8), ("1", 10u8)]` |
| `a_real_aomenc_stream_whose_frame_edge_partition_bit_is_horz_decodes_pixel_exact` | T1 | `0` and `1` | `stream.rs:39945` `for tx_search in [0usize, 1] {` |

**Not one of the ten is genuinely runtime-dependent on an unknown argument.**
Every binding is a literal `0`/`1` (or a `bool` that becomes one) enumerated in
the gate's own text. Each of the ten therefore builds BOTH a `=0` and a `=1`
stream on some attempt, so each is an ON gate under this file's standing "any
spelling that says on wins" rule.

## 3. The true counts

`cargo test -p ec-av1 --lib gate_coverage::tests::print_tx_size_search_census -- --nocapture`:

```
enable-tx-size-search over 230 census-selected gate bodies:
  =0 only 86, =1 only 13, both 17, unresolvable 0, not named 114
```

- **unresolvable: 0 of 230 (0.0%)** — the detector now classifies every body
  that names the flag in any form. That is the headline: the floor is a
  measurement.
- Gates naming the flag at all: 116 of 230 (50.4%). The remaining 114 leave it
  off the command line, which under the "defaulted means unknown" rule stays
  unknown and retires nothing.
- Of the 116: 86 reach only `0`, 13 reach only `1`, **17 reach both** (the ten
  template gates plus seven that spell both literals in one segment).
- Net: 30 gates positively enable the tool, versus the 15 the literal-only
  reader saw. r3's "88 `=0`, 15 `=1`, 9 unknown" is superseded in both
  directions — the `=0` count falls (a gate whose other arm is `=1` is not an
  "off" gate) and the `=1` count doubles.

The count now lives in a test, not a comment, and the test asserts
`unresolvable == 0`, so a new helper-parameter blind spot of this shape fails
loudly instead of quietly restoring a floor.

### The detector change

`spelling_values` (new) merges literal `--flag=<digit>` spellings with every
value a `format!` template resolves to; `bound_values` (new) does the binding
resolution over the four shapes; `resolved_on_state` (new) replaces
`default_on_state`, which is deleted (it became dead — the clean cutover, no
shim left behind). Three details that are load-bearing:

- **Quoted-only for `let` bindings.** A `let tx_search = if attempt % 2 == 0 { "0" } else { "1" };`
  initializer contains the bare integer `2`; reading it as a flag value would
  invent an ON.
- **Position-aware for tuple loops.** `for (tx_search, bit_depth) in [("1", 8u8), …]`
  must read only column 0 — the bit-depth column (`8`, `10`) is not a flag value.
- **Typed-literal suffixes.** `0usize`/`8u8`/`1u32` are single tokens; a
  `bare_digits` that rejected them would have missed `for txs in [0usize, 1]`
  and filed the palette gate as `=1` only. Multi-digit `10u8` is still rejected.

## 4. RED-BEFORE, both directions

Both mutations below were applied to the detector, run, and reverted; the
working tree after revert is byte-identical to the pre-mutation file
(`diff` clean), and all 12 `gate_coverage` tests are green.

**Direction 1 — a mutated `format!` value must MOVE the classification.**
Mutating the template needle so the resolver cannot see
(`format!("--{spelling}=DISABLED{spelling}=")`) reds **two** tests:

- `the_resolver_binds_a_format_built_flag_in_both_directions`: the palette gate
  resolves to `{}` instead of `{'0','1'}`.
- `print_tx_size_search_census`: `unresolvable 9` (not 0), and the split
  degrades to `=0 only 86, =1 only 13, both 8` — 9 of the 17 both-value gates
  silently become "not named", which is precisely the old behaviour.

The same test also mutates the GATE side: for each of three template shapes it
takes the real gate body, rewrites only the bound value
(`[0usize, 1]`→`[0usize, 0]`, `"1"`→`"0"` in the else arm, the tuple table's
`"1"`→`"0"`), and asserts the resolved set moved from `{'0','1'}` to `{'0'}`.
Without that half, a resolver that ignored the binding and returned a constant
would pass.

**Direction 2 — a spelling the old detector already saw must STILL classify.**
Mutating the literal needle instead (`"--DISABLED{spelling}="`) reds **four**:
the two `never_on_*_matches_the_gate_recipes` tests (every tool drops to
"never on"), `print_tx_size_search_census` (`=0 only 0, =1 only 0`), and the
same resolver test, which walks every gate that literally spells
`--enable-tx-size-search=1` and asserts each is still classified on, with a
floor of 10 such gates so the walk cannot pass vacuously. The new arm is
additive over the old one; a resolver that replaced rather than extended the
literal path fails here.

## 5. The entry decision: RETIRED

The tool is exercised by a gate that **asserts the parsed tx-size-search
behaviour**, not merely spells it:

`a_real_aomenc_stream_with_a_1d_tx_class_on_a_rect_transform_decodes_pixel_exact`
spells `--enable-tx-size-search=1` as a literal and hard-asserts
`compared_tx_depths > 0` with the message *"never produced a nonzero tx depth on
a compared attempt — the flag did not arrive"*. `tx_depth` is read **only**
under the frame header's `tx_mode_select` bit (spec 5.11.16; `decode.rs` guards
the read with `tx_select_inter`), so a nonzero `TX_DEPTH_HITS` delta IS the
parsed tx-size-search behaviour, not a proxy for it.

**Measured run of that gate on this tree** (oracle aom `v3.13.3-7-g9bb526a`,
one named test, 3.9 s):

```
12 pixel-exact decodes (0 named refusals out of 12 attempts, 6 of the matches 10-bit),
rect coefficient TUs on compared attempts: 219 (10 of them a 1D tx class);
flag arrival: rect partitions 120, tx depths 53, 1D-class (square) transforms 13
```

`tx depths 53` on compared attempts, and 6 of the 12 pixel-exact matches are
10-bit — so the tool is exercised at **both** depths and the assertion is not
vacuous.

The stronger witness, cited in the entry as well:
`tx_select_inter_gate` (behind
`a_real_aomenc_inter_sequence_with_tx_select_decodes_pixel_exact{,_10bit}`)
**deliberately omits** the flag — its recipe carries the comment
`// DELIBERATELY ABSENT: --enable-tx-size-search=0.` — and asserts
`decode::txfm_split_hits()` moved, i.e. that spec 5.11.17's recursive
`txfm_split` var-tx tree was read. An inter transform is then a split tree
rather than one whole-block transform, which is the entire content of the tool.
That gate's own doc says an unfired gate is a vacuous gate: it panics if no
attempt takes a split.

### In-file reason text, as landed

The `DEFAULT_ON_TOOLS` entry's comment now reads (abridged; full text in the
file above `DEFAULT_ON_TOOLS`):

> `enable-tx-size-search` SETTLED 2026-09-29 (lane-av1txsearch), by measurement
> rather than by a name-shaped guess. The r3 note here said the count was a
> FLOOR: 9 gate bodies built the flag into a `format!` variable that neither
> `flags_in` nor `settings_in` could see … The method r3 asked for is now
> `spelling_values` → `bound_values`, and it resolves all of them;
> `print_tx_size_search_census` asserts the unresolvable count is ZERO, so the
> floor cannot come back unnoticed.
>
> THE ENTRY IS RETIRED, on an ARRIVAL ASSERT rather than on the spelling.
> `a_real_aomenc_stream_with_a_1d_tx_class_on_a_rect_transform_decodes_pixel_exact`
> spells `--enable-tx-size-search=1` as a literal and hard-asserts
> `compared_tx_depths > 0` … `tx_depth` is read ONLY under the frame header's
> `tx_mode_select` bit (spec 5.11.16 …), so a nonzero counter IS the parsed
> tx-size-search behaviour, not a proxy for it. Measured run of that gate on
> this tree (aom 3.13.3): 12 pixel-exact decodes, 0 named refusals, 6 of the 12
> at 10 BIT, 219 rect coefficient TUs on compared attempts, and `flag arrival:
> … tx depths 53` …

The "all 49" prose count stays deleted, per r2; the module doc now points at
the test rather than a number that rots.

## What is not claimed

- The 114 bodies that leave the flag off the command line stay **unknown**, not
  "exercised". The default of `1` is not a stream carrying the tool.
- `bound_values` returns `None` when the variable is a shared helper's
  parameter (bound at a call site in a different segment). That path is
  implemented and reported, not guessed — and today no
  `--enable-tx-size-search` site takes it. A future helper-parameter flag will
  be counted as unresolvable and will red the census test rather than pass
  silently.
- No claim is made about any OTHER `DEFAULT_ON_TOOLS` tool's floor. The
  resolver is general, so those counts also moved; re-run
  `print_never_on_per_bit_depth` to read them. The 8-bit and 10-bit
  `NEVER_ON_*` lists are empty on this tree and their tests are green.
- No decoder behaviour, counter, gate or fixture changed. `stream.rs` is
  untouched.

## Test evidence

`gate_coverage` module, 12 tests, all green on the final tree:

```
test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 704 filtered out
```

Covered by the above: `every_gate_disabling_a_tool_is_a_listed_coverage_hole`,
`tool_settings_reads_whole_flags`,
`the_detector_sees_the_gates_the_spelling_filter_missed` (r3's, still green),
`never_exercised_{8bit,10bit}_matches_the_gate_recipes`,
`never_on_{8bit,10bit}_matches_the_gate_recipes`,
`default_on_tools_do_not_duplicate_the_universe`, the three print tests, and the
two new ones (`print_tx_size_search_census`,
`the_resolver_binds_a_format_built_flag_in_both_directions`).
