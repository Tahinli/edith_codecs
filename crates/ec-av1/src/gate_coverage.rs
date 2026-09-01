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
const NEVER_EXERCISED: &[(&str, &str)] = &[(
    "enable-intrabc",
    "intra block copy syntax is consumed but no block is reconstructed from it",
)];

#[cfg(test)]
mod tests {
    use super::NEVER_EXERCISED;
    use std::collections::{BTreeMap, BTreeSet};

    /// `--enable-*` settings per real-aomenc gate, derived from the gate
    /// source: `flag -> (turned off in N gates, turned on in N, defaulted in N)`.
    fn tool_settings() -> (usize, BTreeMap<String, (usize, usize, usize)>) {
        let src = include_str!("stream.rs");
        // A real-aomenc gate is a `fn` body carrying the shared `--passes=1`
        // every one of them passes. Split on the attribute that opens a test.
        let gates: Vec<&str> = src
            .split("\n    #[")
            .filter(|body| body.contains("\"--passes=1\""))
            .collect();
        assert!(
            gates.len() >= 20,
            "expected the real-aomenc gates, found {}",
            gates.len()
        );

        let mut per_tool: BTreeMap<String, (usize, usize, usize)> = BTreeMap::new();
        let mut names: BTreeSet<String> = BTreeSet::new();
        let mut seen: Vec<BTreeMap<String, char>> = Vec::new();
        for gate in &gates {
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
                names.insert(flag.to_owned());
                here.insert(flag.to_owned(), value);
            }
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
}
