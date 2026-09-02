//! A guard over the real-aomenc gates' own encoder recipes.
//!
//! Every gate in [`crate::stream`] that runs a real `aomenc` picks its coding
//! tools with `--enable-*=0/1` flags, and a flag left off the command line
//! takes aomenc's default (on, for the tools this decoder cares about). That
//! makes it possible -- and it happened -- for a coding tool to be switched
//! *off in every single gate*, so that no real stream in this repository ever
//! exercised it and a corner-cut in the decoder survived twenty gates
//! unnoticed: `reconstruct`'s `smooth_neighbor` was hardcoded `false`, wrong
//! whenever a directional block neighbours a smooth-mode one, and invisible
//! because all twenty gates passed `--enable-smooth-intra=0`
//! (lane-chroma r1, 2026-08-30).
//!
//! The test below re-derives that set from the gate source and pins it. A tool
//! that is switched off in every gate and on in none is *provably never
//! exercised* by a real stream, and belongs on [`NEVER_EXERCISED`] with a
//! reason. Landing the decode support for one means enabling it in that
//! feature's gate, which shrinks the derived set and fails this test until the
//! list is updated -- the point being that the shrink is noticed.

// TILING is deliberately NOT in this derivation (lane-tiles r11): the check
// keys on `--enable-<tool>=0/1`, and `--tile-columns=<log2>` has no such
// shape -- a `--tile-columns=` presence check would also read `=0` (one tile)
// as coverage, which is the opposite of what it would claim. It needs no
// entry either: 13 gates spell `--tile-columns`/`--tile-rows` with a nonzero
// log2 and assert the parsed `tile_info` really carries the grid, and
// `run_multi_tile_gate` covers a 2D grid with the coding tools ON at both
// bit depths.

/// aomenc coding tools that no real-`aomenc` gate is proven to exercise:
/// switched off in every gate that names them, and on in none.
///
/// A gate that simply leaves the flag off its command line does NOT close the
/// hole. aomenc's default for these is content-dependent -- palette and
/// intrabc only come on for screen content -- so "defaulted" means "unknown",
/// not "exercised", and treating it as coverage would retire the entry
/// without a single stream proving the decoder ever saw the tool. Only an
/// explicit `--enable-<tool>=1` in a gate that then asserts the feature fired
/// closes one.
///
/// Each entry is `(flag, why)`. Removing an entry is how a lane records that
/// its tool is now covered by a real stream.
#[cfg(test)]
const NEVER_EXERCISED: &[(&str, &str)] = &[
    // lane-intrabc r1: the DV itself (mv stack against INTRA_FRAME, `ndvc`
    // full-pel read, `av1_find_ref_dv` fallback) and the block-copy
    // prediction are now decoded, but every real aomenc intrabc block this
    // lane could produce sits under TX_MODE_SELECT, whose inter var-tx
    // partition tree is unread -- so no stream reconstructs one end to end
    // yet and this entry stays.
    (
        "enable-intrabc",
        "the block vector is decoded and predicted from, but every real stream's intrabc block needs the unread inter var-tx transform tree",
    ),
];

/// The `--enable-*` tools this decoder cares about, whether or not any gate
/// names one. A flag no gate mentions is *defaulted*, i.e. unknown, and by the
/// rule above unknown is not coverage -- so the universe is fixed here rather
/// than derived from the gate source, which would silently shrink to whatever
/// the gates happen to spell.
#[cfg(test)]
const TOOL_UNIVERSE: &[&str] = &[
    "enable-1to4-partitions",
    "enable-ab-partitions",
    "enable-angle-delta",
    "enable-cdef",
    "enable-cfl-intra",
    "enable-dist-wtd-comp",
    "enable-dual-filter",
    "enable-filter-intra",
    "enable-flip-idtx",
    "enable-global-motion",
    "enable-interintra-comp",
    "enable-intra-edge-filter",
    "enable-intrabc",
    "enable-masked-comp",
    "enable-obmc",
    "enable-order-hint",
    "enable-paeth-intra",
    "enable-palette",
    "enable-rect-partitions",
    "enable-rect-tx",
    "enable-ref-frame-mvs",
    "enable-restoration",
    "enable-smooth-intra",
    "enable-superres",
    "enable-tx64",
    "enable-warped-motion",
];

/// How a non-boolean aomenc knob reads as "the tool is on".
#[cfg(test)]
#[derive(Clone, Copy, PartialEq)]
enum On {
    /// Any non-zero value enables the tool (`--tile-columns=1`, `--loopfilter-control=1`).
    NonZero,
    /// The tool's search only runs at or below this speed (`--cpu-used`).
    AtMost(u32),
}

/// Coding tools aomenc drives through a spelling [`TOOL_UNIVERSE`] does not
/// cover, listed with their aomenc default.
///
/// lane-covbd's derivation read `--enable-*` flags only, so a tool a gate pins
/// off through another spelling was invisible: all 49 gates that name it pass
/// `--enable-tx-size-search=0`, and the real-stream PANIC hiding behind that
/// pin was found by lane-ab16, not by this guard. `--loopfilter-control=0`,
/// `--tile-columns=0` and a `--cpu-used` high enough to switch a search off are
/// the same shape.
///
/// Each entry is `(tool, spellings, aomenc default, what counts as on)`. The
/// "defaulted means unknown" rule of [`NEVER_EXERCISED`] applies unchanged: a
/// default of `1` does not prove the encoder picked the tool for any stream, so
/// only a gate that spells an on-value at that bit depth retires an entry.
#[cfg(test)]
const DEFAULT_ON_TOOLS: &[(&str, &[&str], &str, On)] = &[
    ("enable-tx-size-search", &["enable-tx-size-search"], "1", On::NonZero),
    ("enable-directional-intra", &["enable-directional-intra"], "1", On::NonZero),
    ("enable-smooth-interintra", &["enable-smooth-interintra"], "1", On::NonZero),
    ("enable-interintra-wedge", &["enable-interintra-wedge"], "1", On::NonZero),
    ("enable-diff-wtd-comp", &["enable-diff-wtd-comp"], "1", On::NonZero),
    ("enable-onesided-comp", &["enable-onesided-comp"], "1", On::NonZero),
    ("enable-fwd-kf", &["enable-fwd-kf"], "0", On::NonZero),
    ("deblocking", &["loopfilter-control"], "1", On::NonZero),
    ("multi-tile", &["tile-columns", "tile-rows"], "0", On::NonZero),
    ("intrabc-search", &["cpu-used"], "0", On::AtMost(2)),
];

/// Non-`--enable-*` spellings that drive a [`TOOL_UNIVERSE`] tool.
///
/// `--superres-mode=1` is how all three superres gates switch superres on;
/// without this map they read as an `enable-superres` hole while a real stream
/// exercises the tool (lane-covbd deferred exactly this).
#[cfg(test)]
const ALIASES: &[(&str, &str)] = &[("superres-mode", "enable-superres")];

/// Coverage is per (tool x bit depth), not per tool.
///
/// lane-hbdinter found two 10-bit-only defects (SGR box sums unscaled, Wiener
/// clamp pinned at the 8-bit bound) that survived the "10-bit bit-exact"
/// milestone because every 10-bit gate passed `--enable-restoration=0`:
/// `enable-restoration` is positively exercised -- at 8 bits only. A tool the
/// 8-bit gates cover says nothing about the high-bit-depth path through the
/// same code, so the two lists below are pinned separately.
///
/// Each entry is `(flag, why)`. A gate that passes `--enable-<tool>=1` at that
/// depth retires the entry, and this test fails until it is deleted.
#[cfg(test)]
const NEVER_EXERCISED_8BIT: &[(&str, &str)] = &[
    (
        "enable-cfl-intra",
        "off in 41 gates, on in none: chroma-from-luma prediction is unimplemented",
    ),
    (
        "enable-dist-wtd-comp",
        "off in 11 gates, on in none: distance-weighted compound is unimplemented",
    ),
    (
        "enable-dual-filter",
        "never spelled by any gate, so defaulted = unknown; no stream is proven to carry per-direction interp filters",
    ),
    (
        "enable-flip-idtx",
        "never spelled; the flip/identity transform types are unproven by a real stream",
    ),
    (
        "enable-global-motion",
        "off in 5 gates, on in none at 8 bits; lane-gm owns the global-warp prediction that is still missing",
    ),
    (
        "enable-intrabc",
        "the block vector is decoded and predicted from, but every real stream's intrabc block needs the unread inter var-tx transform tree",
    ),
    (
        "enable-rect-tx",
        "never spelled; rect transforms reach the decoder only through partition shape, never through a gate that names the tool",
    ),
];

#[cfg(test)]
const NEVER_EXERCISED_10BIT: &[(&str, &str)] = &[    // lane-cwarp's 10-bit compound-global-warp gate closed `enable-global-motion`
    // and `enable-dist-wtd-comp` at 10 bits without deleting their entries here,
    // so this list was already stale (and this test already red) at main 9c35ecc;
    // lane-tiles r11's multi-tile 10-bit gates then closed `enable-ab-partitions`,
    // `enable-rect-partitions` and `enable-restoration` at 10 bits. All five
    // deleted together.

    // Only 4 of the 45 real-aomenc gates encode at 10 bits, and they pin the
    // pixel filters and the intra tool set off. Every entry the 8-bit list
    // carries is a hole here too. lane-hbdgates r1 closed six of the seven
    // 8-bit-only entries with real 10-bit gates (filter-intra, smooth-intra,
    // paeth-intra, intra-edge-filter, rect-partitions, ab-partitions); its
    // seventh, the 10-bit LR gate, was `#[ignore]`d on that branch and passes
    // un-ignored on main, which carries the fix.
    // `enable-restoration` LEFT this list on 2026-09-01: lane-hbdinter's
    // 10-bit inter gate passes `--enable-restoration=1` and asserts a real
    // Wiener/SGR unit fired, which is what caught the two defects (SGR box
    // sums never brought back to the 8-bit scale, Wiener clamp at the wrong
    // bound). `enable-dist-wtd-comp` and `enable-global-motion` left it the
    // same day for the same reason: lane-cwarp's 10-bit compound global-warp
    // gate passes `=1` for both. `enable-1to4-partitions` left it on 2026-09-02:
    // lane-tx64x16 r4's 32-level 1:4 gate has a 10-bit arm that asserts both
    // orientations and coded strips inside pixel-exact attempts.
    (
        "enable-cfl-intra",
        "hole at both depths, see the 8-bit list",
    ),
    (
        "enable-dual-filter",
        "hole at both depths, see the 8-bit list",
    ),
    // `enable-flip-idtx` LEFT this list on 2026-09-02 (lane-rect1d r1):
    // `a_real_aomenc_stream_with_a_1d_tx_class_on_a_rect_transform_decodes_pixel_exact`
    // passes `=1` at both depths and pixel-compares six 10-bit decodes, so the
    // flip/identity/1D transform types are proven to reach a real 10-bit stream
    // this decoder reconstructs exactly (10 of its rect coefficient TUs carry a
    // 1D tx class).
    ("enable-intrabc", "hole at both depths, see the 8-bit list"),
    ("enable-rect-tx", "hole at both depths, see the 8-bit list"),
];

/// [`DEFAULT_ON_TOOLS`] entries no 8-bit gate spells on, with the reason.
///
/// Each entry is `(tool, why)`; the reason states whether the tool is pinned
/// OFF in every gate that names it (a hard hole -- no stream can carry it) or
/// merely defaulted (unknown, and unknown is not coverage).
#[cfg(test)]
const NEVER_ON_8BIT: &[(&str, &str)] = &[];

/// [`DEFAULT_ON_TOOLS`] entries no 10-bit gate spells on, with the reason.
#[cfg(test)]
const NEVER_ON_10BIT: &[(&str, &str)] = &[];

#[cfg(test)]
mod tests {
    use super::{
        ALIASES, DEFAULT_ON_TOOLS, NEVER_EXERCISED, NEVER_EXERCISED_8BIT, NEVER_EXERCISED_10BIT,
        NEVER_ON_10BIT, NEVER_ON_8BIT, On, TOOL_UNIVERSE,
    };
    use std::collections::{BTreeMap, BTreeSet};

    /// Bodies of the real-aomenc gates: a `fn` body carrying the shared
    /// `--passes=1` every one of them passes. Split on the attribute that
    /// opens a test.
    fn gate_bodies() -> Vec<&'static str> {
        let src = include_str!("stream.rs");
        let gates: Vec<&str> = src
            .split("\n    #[")
            // lane-hbdgates r1: an `#[ignore]`d gate exercises nothing, so it
            // must not close a hole. Its body is the segment that opens with
            // the ignore attribute.
            .filter(|body| !body.starts_with("ignore"))
            // lane-hbdgates r1: gates that build their stream through the
            // shared 10-bit helpers spell `--passes=1` there, not inline.
            .filter(|body| {
                body.contains("\"--passes=1\"")
                    || body.contains("ten_bit_tool_gate(")
                    || body.contains("encode_10bit_gradients")
            })
            .collect();
        assert!(
            gates.len() >= 20,
            "expected the real-aomenc gates, found {}",
            gates.len()
        );
        gates
    }

    /// `--enable-<flag>=<value>` settings spelled inside one gate body.
    fn flags_in(gate: &str) -> BTreeMap<String, char> {
        let mut here = BTreeMap::new();
        for (i, _) in gate.match_indices("\"--enable-") {
            let rest = &gate[i + 3..];
            let Some(end) = rest.find('"') else { continue };
            let Some((flag, value)) = rest[..end].split_once('=') else {
                continue;
            };
            let Some(value) = value.chars().next() else {
                continue;
            };
            here.insert(flag.to_owned(), value);
        }
        here
    }

    /// Every `--flag=value` spelled inside one gate body, values kept whole
    /// (`--cpu-used=4`, `--tile-columns=2`), not just their first character.
    fn settings_in(gate: &str) -> BTreeMap<String, String> {
        let mut here = BTreeMap::new();
        for (i, _) in gate.match_indices("\"--") {
            let rest = &gate[i + 3..];
            let Some(end) = rest.find('"') else { continue };
            let Some((flag, value)) = rest[..end].split_once('=') else {
                continue;
            };
            here.insert(flag.to_owned(), value.to_owned());
        }
        here
    }

    /// Whether a gate body spells one [`DEFAULT_ON_TOOLS`] entry on / off.
    fn default_on_state(gate: &str, spellings: &[&str], on: On) -> Option<bool> {
        let here = settings_in(gate);
        let mut state = None;
        for spelling in spellings {
            let Some(value) = here.get(*spelling) else {
                continue;
            };
            let Ok(value) = value.parse::<u32>() else {
                continue;
            };
            let is_on = match on {
                On::NonZero => value != 0,
                On::AtMost(limit) => value <= limit,
            };
            // Any spelling that says "on" wins: --tile-columns=0 --tile-rows=1
            // is a multi-tile stream.
            state = Some(state.unwrap_or(false) || is_on);
        }
        state
    }

    /// `--enable-*` settings per real-aomenc gate, derived from the gate
    /// source: `flag -> (turned off in N gates, turned on in N, defaulted in N)`.
    fn tool_settings() -> (usize, BTreeMap<String, (usize, usize, usize)>) {
        let gates = gate_bodies();

        let mut per_tool: BTreeMap<String, (usize, usize, usize)> = BTreeMap::new();
        let mut names: BTreeSet<String> = BTreeSet::new();
        let mut seen: Vec<BTreeMap<String, char>> = Vec::new();
        for gate in &gates {
            let here = flags_in(gate);
            names.extend(here.keys().cloned());
            seen.push(here);
        }
        for name in names {
            let off = seen.iter().filter(|g| g.get(&name) == Some(&'0')).count();
            let on = seen.iter().filter(|g| g.get(&name) == Some(&'1')).count();
            per_tool.insert(name, (off, on, gates.len() - off - on));
        }
        (gates.len(), per_tool)
    }

    #[test]
    fn every_gate_disabling_a_tool_is_a_listed_coverage_hole() {
        let (gate_count, per_tool) = tool_settings();
        // A hole OPENS when every gate names the flag and every one of them
        // says 0 -- then no real stream can exercise the tool.
        let derived: BTreeSet<&str> = per_tool
            .iter()
            .filter(|&(_, &(off, on, defaulted))| off == gate_count && on == 0 && defaulted == 0)
            .map(|(name, _)| name.as_str())
            .collect();
        // A hole CLOSES only on positive evidence: some gate passes `=1`.
        // A gate that merely leaves the flag off its command line proves
        // nothing -- aomenc's default for palette and intrabc is content-
        // dependent -- so "defaulted" must not retire an entry, or a listing
        // would disappear the moment an unrelated gate is added.
        let exercised: BTreeSet<&str> = per_tool
            .iter()
            .filter(|&(_, &(_, on, _))| on > 0)
            .map(|(name, _)| name.as_str())
            .collect();
        let listed: BTreeSet<&str> = NEVER_EXERCISED.iter().map(|&(flag, _)| flag).collect();

        let unlisted: Vec<&&str> = derived.difference(&listed).collect();
        assert!(
            unlisted.is_empty(),
            "these aomenc tools are switched off in all {gate_count} real-aomenc gates and on in \
             none, so no real stream exercises them, and they are not listed in NEVER_EXERCISED: \
             {unlisted:?} -- either enable the tool with `=1` in its own gate or add it to the \
             list with a reason"
        );
        let stale: Vec<&&str> = listed.intersection(&exercised).collect();
        assert!(
            stale.is_empty(),
            "NEVER_EXERCISED lists {stale:?}, but a gate now passes `=1` for them -- delete \
             those entries, the coverage hole is closed"
        );
    }

    /// The derivation is only meaningful if a flag left off a gate's command
    /// line really does mean "aomenc's default", i.e. the parser above is
    /// reading whole flags and not fragments.
    #[test]
    fn tool_settings_reads_whole_flags() {
        let (gate_count, per_tool) = tool_settings();
        assert!(
            per_tool.contains_key("enable-cdef"),
            "expected --enable-cdef among the gate flags"
        );
        for (name, &(off, on, defaulted)) in &per_tool {
            assert!(
                !name.is_empty() && !name.contains(['"', '=']),
                "malformed flag {name:?}"
            );
            assert_eq!(
                off + on + defaulted,
                gate_count,
                "{name} counts do not cover every gate"
            );
        }
    }

    /// A gate encodes at 10 bits when its recipe says so: the shared 10-bit
    /// helper, an explicit `--bit-depth=10`/`--input-bit-depth=10`, or a
    /// `yuv420p10le` fixture handed to aomenc. Everything else is aomenc's
    /// 8-bit default.
    /// lane-defon r1: a gate helper parameterised on `bit_depth` builds BOTH
    /// an 8-bit and a 10-bit stream from one recipe, so its flags cover both
    /// depths -- classifying it by the 10-bit strings inside its conditional
    /// alone hid `--enable-tx-size-search=1` from the 8-bit list.
    fn covers_both_depths(body: &str) -> bool {
        body.contains("if bit_depth == 10")
    }

    /// Whether a gate body drives a stream at this depth.
    fn covers_depth(body: &str, ten_bit: bool) -> bool {
        covers_both_depths(body) || is_ten_bit(body) == ten_bit
    }

    fn is_ten_bit(body: &str) -> bool {
        body.contains("encode_10bit_gradients")
            || body.contains("ten_bit_tool_gate(")
            || body.contains("--bit-depth=10")
            || body.contains("--input-bit-depth=10")
            || body.contains("yuv420p10le")
    }

    /// Flags a gate positively enables (`=1`), at gates of the given depth.
    fn enabled_at(ten_bit: bool) -> BTreeSet<String> {
        let mut on = BTreeSet::new();
        for gate in gate_bodies() {
            if !covers_depth(gate, ten_bit) {
                continue;
            }
            for (flag, value) in flags_in(gate) {
                if value == '1' {
                    on.insert(flag);
                }
            }
            // A tool driven by its own spelling counts as exercised.
            let here = settings_in(gate);
            for (alias, tool) in ALIASES {
                if here.get(*alias).is_some_and(|v| v != "0") {
                    on.insert((*tool).to_owned());
                }
            }
        }
        on
    }

    /// The tools of [`TOOL_UNIVERSE`] that no gate of this depth enables.
    fn never_exercised_at(ten_bit: bool) -> BTreeSet<&'static str> {
        let on = enabled_at(ten_bit);
        TOOL_UNIVERSE
            .iter()
            .copied()
            .filter(|t| !on.contains(*t))
            .collect()
    }

    fn check_depth(ten_bit: bool, listed: &[(&str, &str)]) {
        let derived = never_exercised_at(ten_bit);
        let listed: BTreeSet<&str> = listed.iter().map(|&(flag, _)| flag).collect();
        let depth = if ten_bit { "10-bit" } else { "8-bit" };
        let unlisted: Vec<&&str> = derived.difference(&listed).collect();
        assert!(
            unlisted.is_empty(),
            "no {depth} gate passes `=1` for {unlisted:?}, so no real {depth} stream exercises \
             them -- enable the tool in a {depth} gate that asserts it fired, or list it"
        );
        let stale: Vec<&&str> = listed.difference(&derived).collect();
        assert!(
            stale.is_empty(),
            "the {depth} list still names {stale:?}, but a {depth} gate now passes `=1` for them \
             -- delete those entries, the coverage hole is closed"
        );
    }

    #[test]
    fn never_exercised_8bit_matches_the_gate_recipes() {
        check_depth(false, NEVER_EXERCISED_8BIT);
    }

    #[test]
    fn never_exercised_10bit_matches_the_gate_recipes() {
        check_depth(true, NEVER_EXERCISED_10BIT);
    }

    /// `cargo test -p ec-av1 --lib gate_coverage -- --nocapture` prints both
    /// lists, so the per-depth holes can be read without opening this file.
    #[test]
    fn print_never_exercised_per_bit_depth() {
        let ten: Vec<&str> = gate_bodies()
            .into_iter()
            .filter(|b| is_ten_bit(b))
            .collect();
        let total = gate_bodies().len();
        println!(
            "gate_coverage: {} real-aomenc gates, {} of them 10-bit",
            total,
            ten.len()
        );
        for (label, ten_bit) in [("8BIT", false), ("10BIT", true)] {
            let holes = never_exercised_at(ten_bit);
            println!(
                "NEVER_EXERCISED_{label} ({} of {}):",
                holes.len(),
                TOOL_UNIVERSE.len()
            );
            for flag in &holes {
                println!("    --{flag}");
            }
        }
    }

    /// [`DEFAULT_ON_TOOLS`] per depth: `tool -> (off in N gates, on in N, defaulted in N)`.
    fn default_on_settings(ten_bit: bool) -> (usize, BTreeMap<&'static str, (usize, usize, usize)>) {
        let gates: Vec<&str> = gate_bodies()
            .into_iter()
            .filter(|b| covers_depth(b, ten_bit))
            .collect();
        let mut per_tool = BTreeMap::new();
        for &(tool, spellings, _, on) in DEFAULT_ON_TOOLS {
            let states: Vec<Option<bool>> = gates
                .iter()
                .map(|g| default_on_state(g, spellings, on))
                .collect();
            let turned_on = states.iter().filter(|s| **s == Some(true)).count();
            let turned_off = states.iter().filter(|s| **s == Some(false)).count();
            per_tool.insert(
                tool,
                (turned_off, turned_on, gates.len() - turned_off - turned_on),
            );
        }
        (gates.len(), per_tool)
    }

    /// The [`DEFAULT_ON_TOOLS`] no gate of this depth positively enables.
    fn never_on_at(ten_bit: bool) -> BTreeSet<&'static str> {
        default_on_settings(ten_bit)
            .1
            .into_iter()
            .filter(|&(_, (_, on, _))| on == 0)
            .map(|(tool, _)| tool)
            .collect()
    }

    fn check_default_on(ten_bit: bool, listed: &[(&str, &str)]) {
        let derived = never_on_at(ten_bit);
        let listed: BTreeSet<&str> = listed.iter().map(|&(tool, _)| tool).collect();
        let depth = if ten_bit { "10-bit" } else { "8-bit" };
        let unlisted: Vec<&&str> = derived.difference(&listed).collect();
        assert!(
            unlisted.is_empty(),
            "no {depth} gate spells an on-value for {unlisted:?}, so no real {depth} stream is \
             proven to carry them -- switch the tool on in a {depth} gate that asserts it fired, \
             or list it with a reason"
        );
        let stale: Vec<&&str> = listed.difference(&derived).collect();
        assert!(
            stale.is_empty(),
            "the {depth} default-on list still names {stale:?}, but a {depth} gate now switches \
             them on -- delete those entries, the coverage hole is closed"
        );
    }

    #[test]
    fn never_on_8bit_matches_the_gate_recipes() {
        check_default_on(false, NEVER_ON_8BIT);
    }

    #[test]
    fn never_on_10bit_matches_the_gate_recipes() {
        check_default_on(true, NEVER_ON_10BIT);
    }

    /// A tool must not be pinned twice: [`TOOL_UNIVERSE`] already derives the
    /// `--enable-*` spellings under the `=1` rule.
    #[test]
    fn default_on_tools_do_not_duplicate_the_universe() {
        for &(tool, spellings, _, _) in DEFAULT_ON_TOOLS {
            assert!(
                !TOOL_UNIVERSE.contains(&tool),
                "{tool} is already covered by TOOL_UNIVERSE"
            );
            assert!(!spellings.is_empty(), "{tool} has no aomenc spelling");
        }
        for (alias, tool) in ALIASES {
            assert!(
                TOOL_UNIVERSE.contains(tool),
                "alias --{alias} names {tool}, which is not a TOOL_UNIVERSE tool"
            );
        }
    }

    /// `cargo test -p ec-av1 --lib gate_coverage -- --nocapture` prints the
    /// default-on holes with their off/defaulted split.
    #[test]
    fn print_never_on_per_bit_depth() {
        for (label, ten_bit) in [("8BIT", false), ("10BIT", true)] {
            let (gate_count, per_tool) = default_on_settings(ten_bit);
            let holes = never_on_at(ten_bit);
            println!(
                "NEVER_ON_{label} ({} of {}, over {gate_count} {label} gates):",
                holes.len(),
                DEFAULT_ON_TOOLS.len()
            );
            for tool in &holes {
                let (off, _, defaulted) = per_tool[tool];
                let spellings = DEFAULT_ON_TOOLS
                    .iter()
                    .find(|e| e.0 == *tool)
                    .map(|e| e.1.join(","))
                    .unwrap_or_default();
                println!(
                    "    {tool} (--{spellings}): off in {off}, defaulted in {defaulted}, on in 0"
                );
            }
        }
    }
}
