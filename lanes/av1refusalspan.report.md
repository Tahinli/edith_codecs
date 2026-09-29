# lane-av1refusalspan — a real function boundary for the refusal inventory's anchor

Base: `745c60e7` (main). Branch `lane-av1refusalspan`, commits `797fa52b` + `566d61bd`.
Worktree `~/.cache/wt/av1refusalspan`, lane-private `CARGO_TARGET_DIR`.

```
cargo test -p ec-av1 --lib refusal_inventory
anchor strength: 26 rows quote the WHOLE refusal string, 7 quote its LEADING CLAUSE,
                 0 match neither: []
refusal inventory: 33 refusals + 1 capability claims, 33 proven
test result: ok. 19 passed; 0 failed; 0 ignored; 0 measured; 733 filtered out
```

No decoder logic changed. The seven decoder/stream gates I touched were each run
individually and are green (`a_var_tx_tree_never_presents_a_leaf_larger_than_the_unit_it_entered`,
`every_intra_in_inter_shape_the_census_lists_has_a_size_group_row`,
`every_rect_transform_shape_the_census_lists_has_a_coefficient_table_and_scan`,
`every_shape_that_allows_motion_variation_has_a_motion_mode_cdf_row`,
`a_selected_reference_with_an_empty_ref_frame_idx_slot_refuses_by_name`,
`every_inter_record_publishes_an_obmc_readable_filter`,
`every_frame_size_a_header_can_code_has_a_mode_info_grid`).

---

## 0. Where this branch sits, stated once

`lane-av1refusal` (tip `910dc296`, r7) is **unmerged**, and main therefore does not
contain the anchor check at all: at `745c60e7` `every_proven_refusal_names_a_test_that_exists`
asks only whether `fn <gate>(` appears *somewhere* in the three files. The window rule the
ticket quotes lives on that branch.

I based this lane on main as instructed, so the work is a **self-contained, corrected
version** of r3–r7's `body_of`: real boundary, missing boundary = failure, leading-clause
anchor, measured distribution. **Merge this one, not both** — they edit the same test and
the same three rows' gates. If `lane-av1refusal` is merged first, take this branch's
`refusal_inventory.rs`, `decode.rs` and `stream.rs` over it and drop r3–r7's `body_of`.

---

## 1. The boundary rule, and why the alternatives lose

**Chosen: the brace that closes the gate's body.** `body_braces` walks Rust from the `fn`
keyword — not lines — and skips string literals, raw strings (`r"`, `r#"…"#`), byte strings,
char literals, lifetimes (`'a`, `'static`), line comments and nestable block comments.
`gate_body` returns the text from the body's `{` to its matching `}`.

The three candidates, and how each fails:

| rule | why it loses on this tree |
|---|---|
| next `#[test]` / `#[cfg(test)]` attribute | it bounds a *test region*, not a function. The attribute-less helpers between two tests (`decode_path_refusals`, `squash_ws`, `corner_scan`-style closures, `reachable`) fall inside the preceding test's window, and — worse — it has no answer for the **last** test in a file, which is the same "no boundary → widen" hole the ticket asks me to close. Attribute order also varies (`#[test]`, `#[ignore]`, doc comments). |
| explicit per-gate span, recorded next to `PROVEN` | a second thing to keep in step with the source, and it fails **open**: when a gate is renamed, moved or deleted, the recorded span still points at *some* text and the row stays green. That is the defect class in a different costume. |
| same-indent `fn` line (what r3 shipped) | keepable as an *invariant*, useless as a *boundary* — see §5's M4. |

Why the brace rule is the right one for this file specifically: `decode.rs` is 56k lines
of gates whose bodies are dense with literal braces — refusal strings carry `{tx_w}`,
`{bw}`, `{part32}`, and asserts carry `{ref_frame}`, `{w}x{h}` — and the file is
re-indented by merges (the wave merge dedented a call site by four spaces). A rule that
counts `{`/`}` as bytes, or that keys on indentation, is reading exactly the parts of
this file that churn.

---

## 2. A missing boundary is a failure, never a wider window

`gate_body` returns `Result<&str, String>` and has exactly four `Err` paths, none of
which touches a byte outside the gate:

1. **no definition** — `fn <gate>(` appears on no definition line (a comment or a string
   that merely names the gate does not count: `gate_definitions` requires the text before
   the keyword to be whitespace and qualifiers only);
2. **never closed** — the scan reaches end of file without the depth returning to zero;
3. **ambiguous** — the name is defined more than once, so "its body" has no referent;
4. **over-reach** — the extracted text still carries another same-indent `fn`, which means
   the scanner under-counted.

`every_proven_refusal_names_a_test_that_exists` collects paths 1–3 into an `unbounded`
list and asserts it empty; path 4 fires inside `gate_body` and surfaces the same way.
A spin in the scanner is a fifth case, and it used to be a **hang** (see §5's M3); every
arm now has to advance the cursor, asserted at the top of the loop.

---

## 3. Re-measured over real bodies

The `25 whole / 8 leading clause / 0 neither` line was, as r7 said, a distribution over
"whatever the window reached". Measured over bodies the brace rule actually finds, on
main's tree **before any gate was touched**:

```
anchor strength: 13 rows quote the WHOLE refusal string, 6 quote its LEADING CLAUSE,
                 14 match neither
```

**14 of 33 rows were anchored by nothing.** Every one of them is now anchored, by the gate
naming the refusal it proves and pinning the guard that still carries the string
(`refusal_inventory::pins_refusal`, whitespace-squashed so a literal written over two
lines with a `\` continuation still matches):

| # | row | gate | before | after |
|---|---|---|---|---|
| 1 | `an intra-coded {bw}x{bh} block on the inter block path (…)` | `every_intra_in_inter_shape_the_census_lists_has_a_size_group_row` | neither | whole |
| 2 | `a rectangular inter luma transform unit whose shape has no coefficient table set here` | `every_rect_transform_shape_the_census_lists_has_a_coefficient_table_and_scan` | neither | whole |
| 3 | `a rectangular inter chroma transform unit … table set here` | same gate | neither | whole |
| 4 | `a rectangular transform unit whose shape has no coefficient scan table here` | same gate | neither | whole |
| 5 | `an inter var-tx tree with a leaf transform larger than 32x32` | `a_var_tx_tree_never_presents_a_leaf_larger_than_the_unit_it_entered` | neither | whole |
| 6 | `… larger than 64x64` | same gate | neither | whole |
| 7 | `a Golomb tail longer than this decoder reads` | `read_golomb_reads_every_value_a_conformant_stream_can_carry` | neither | whole |
| 8 | `a motion_mode symbol for a block shape with no CDF row here` | `every_shape_that_allows_motion_variation_has_a_motion_mode_cdf_row` | neither | whole |
| 9 | `a frame with no mode-info grid` | `every_frame_size_a_header_can_code_has_a_mode_info_grid` | neither | whole |
| 10 | `a reference frame selected with no picture at this frame's own ref_frame_idx slot for it` | `a_selected_reference_with_an_empty_ref_frame_idx_slot_refuses_by_name` | neither | whole (its const was a **prefix**, `…own ref_frame_idx slot`; extended to the whole string) |
| 11 | `an OBMC neighbour whose switchable interp filter was never recorded` | `every_inter_record_publishes_an_obmc_readable_filter` | neither | whole |
| 12 | `a coded HORZ/VERT strip whose chroma transform has no rect coefficient tables here` | `every_rect_strip_shape_the_split_path_codes_has_a_luma_and_chroma_table` | neither | whole |
| 13 | `a split intra strip whose transform unit is {tx_w}x{tx_h} (…)` | same gate | neither | whole |
| 14 | `CfL, filter intra or a palette on a 128-root HORZ/VERT intra block (…)` | `no_128_root_half_reads_a_cfl_filter_intra_or_palette_symbol` | neither | leading clause |

13 whole + 1 clause → the final `26 / 7 / 0`. No row is marked NOT-PROVEN: each of the 14
has a real proof behind it (an enumeration, a census or a negative gate); what was missing
was the gate saying so.

Rows 2–4 and 14 are the three the earlier tightening (r5) had already recovered — see §5.

---

## 4. Non-vacuity of the boundary

Three permanent tests, all in `refusal_inventory::tests`:

- `a_gate_body_is_bounded_by_its_own_closing_brace` — two adjacent functions where the
  **neighbour** carries the string; asserts the gate's body does not and the neighbour's
  does; asserts the **last function in a file** (`fn tail() {}`, nothing after it) closes
  exactly on its own brace (`Ok("{}")`); asserts a body whose closing brace is missing is
  `Err`; and asserts a `{` inside a string, a `'}'` char literal, a `// {` and a `/* }} */`
  comment do not move the boundary.
- `every_named_gate_body_is_bounded_in_these_files` — the same boundary over the three
  real sources: all 33 named gates resolve to exactly one function whose body ends on a
  `}` and holds no other function, and the **last function in each of the three files**
  terminates correctly.
- `a_body_carrying_another_function_is_refused` — the `sibling_fn` predicate on its own
  text (a same-indent `fn` is caught; a deeper-indented nested `fn` is not), so it cannot
  be satisfied by the scanner happening to be right today.

Four mutations, each applied, observed red, reverted (`git status` clean):

| # | mutation | result |
|---|---|---|
| **M1** | delete the anchor from `every_intra_in_inter_shape_the_census_lists_has_a_size_group_row` and park the refusal string as a `let _ = …` **one line past its closing brace**, inside `motion_mode_cdf_row` | RED: `26→25` whole, `0→1` neither, and the row is listed by name — a neighbour's text one line away cannot satisfy a row |
| **M2** | `gate_definitions` accepts column-0 definitions only, so gates inside `mod tests` are invisible | RED: 25 rows reported `… is not defined in decode.rs, stream.rs or refusal_inventory.rs` under the `unbounded` heading, and both boundary tests fail. **This is the missing-boundary-is-a-failure property on real data.** |
| **M3** | the string-literal skip becomes a naive counter (`i += 1`) | RED: `a brace in a literal moved the boundary: Err("the body of this_gate() is never closed -- boundary missing")`. A body that would run into its neighbour fails; it never passes. |
| **M3b** | the string arm becomes a bare `continue` (no cursor advance) | the boundary test **hung** for 600 s instead of failing. That is why `body_braces` now asserts every arm advances — the failure mode moved from a hung suite to a failed one (`566d61bd`). |
| **M4** | `function_body` replaced by the **old rule** (next same-indent `fn`, else end of file) | **GREEN**, and that is a finding, not a pass: on *today's* main tree every one of the 33 gates happens to have a following same-indent `fn`, so the old rule coincides with the true boundary here. It is the same code that produced a 27,427-line window on `lane-av1refusal`'s tree. The old rule's failure is tree-dependent and silent, which is precisely why it is not a rule; the brace rule does not move when the file is reorganised. |

M4 also re-frames `sibling_fn`: it is a backstop against a scanner that under-counts, not
the thing that catches the leak — the leak is caught because the body is the *function's*.

---

## 5. The three rows the earlier tightening recovered

Kept anchored, on their own merits:

- **`a rectangular inter luma transform unit …` / chroma twin / scan twin** → the gate
  `every_rect_transform_shape_the_census_lists_has_a_coefficient_table_and_scan` names all
  three strings and pins all three guard sites.
- **`CfL, filter intra or a palette on a 128-root HORZ/VERT intra block (…)`** →
  `no_128_root_half_reads_a_cfl_filter_intra_or_palette_symbol` carries the full leading
  clause as a const and pins it.
- **`a reference frame selected with no picture at this frame's own ref_frame_idx slot for it`**
  → `a_selected_reference_with_an_empty_ref_frame_idx_slot_refuses_by_name`'s const is now
  the **whole** string, not the prefix `…own ref_frame_idx slot` (the row's anchor is the
  whole string: it has no `(` and no `:`).

Measurement of all three under the real boundary, with their anchors removed: **all three
are in the 14-row `neither` bucket of §3.** That is the direct answer to "is the
tightening earned": under a boundary that is really a function, those rows were anchored by
nothing, and each needed its gate to say which string it was about.

---

## 6. What is deliberately not here

- `PROVEN` is still `&[(&str, &str)]`. The r1/r2 `Proof` tagging (NEGATIVE / ENUMERATION /
  TABLE PIN) and the "every refusal needs a row" direction are `lane-av1refusal`'s scope,
  not this lane's; porting them here would collide on the same table for no gain.
- No refusal, `PROVEN` row or decoder behaviour changed. The only semantic change is that
  nine gates now assert they name the refusal they prove.

## 7. Open for Main

- Merge `lane-av1refusalspan`; retire `lane-av1refusal`'s `body_of` (see §0).
- The new pins make a refusal-string edit in `decode.rs`/`stream.rs` turn **two** gates red
  (the pinning gate and the inventory row). That is intended, but it is the first thing
  that will surprise the next lane that renames a guard message.
