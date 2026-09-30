#![cfg(test)]
//! A measured bound on the crate's BY-VALUE types, and a gate that bites.
//!
//! # The class
//!
//! A large struct returned BY VALUE from a public constructor is a stack
//! cost, not a heap cost, and the cost is paid by the CALLER. The
//! constructors nest, and in an unoptimized build each level materialises
//! its whole `Result<T>` return slot before the inner call returns. So one
//! value's `size_of` is the MULTIPLIER, and the chain length is the other
//! factor.
//!
//! That is not hypothetical. `Decoder` was **10,944** bytes by value --
//! five INLINE `FrameContext` slots (2,039 bytes each: the `frame_ctxs`
//! array of four, plus the `ctx` in force) -- and 10,944 is **1.34x** this
//! crate's 8 KiB by-value budget. Boxing both fields
//! (`frame_ctxs: Box<[FrameContext; 4]>` and `ctx: Box<FrameContext>`) took
//! it to **768**, and [`the_frame_contexts_stay_behind_a_box`] is the pin
//! that keeps it there.
//!
//! # Why this module exists
//!
//! One boxed field is one measurement. This module is the sweep:
//!
//! 1. [`BY_VALUE_TYPES`] is the inventory -- every type in this crate a
//!    public `fn` can return by value, with a measured `size_of` recorded
//!    as of this commit. [`the_measured_inventory_is_accurate`] fails if a
//!    type's size moves, so the table cannot rot into stale small numbers.
//! 2. [`STACK_BUDGET`] is the bound, and
//!    [`every_by_value_type_fits_the_stack_budget`] asserts it BY NAME for
//!    every public row, so a regression reds naming the type.
//! 3. [`the_frame_contexts_stay_behind_a_box`] pins the boxed field, with
//!    a negative control so the pin cannot be matching a comment.
//! 4. [`the_deepest_by_value_constructors_fit_on_a_tight_stack`] is the
//!    capability arm: it really constructs `Decoder` on a deliberately
//!    tight thread and asserts real post-construction state, pinning the
//!    measured live stack of the by-value return path. At 256 KiB it does
//!    NOT red the unboxing mutation -- a 10,944-byte struct is not that
//!    expensive; the budget assert and the box pin are what red, BY NAME.
//!    The arm's own docs carry the measured threshold table.
//! 5. [`every_by_value_public_return_is_in_the_inventory`] re-derives the
//!    set of by-value public returns from the crate source.
//!    [`the_inventory_scan_is_not_vacuous`] is its control.
//!
//! # The bound, and why this number
//!
//! [`STACK_BUDGET`] = 8192 bytes (8 KiB) -- the SAME number as ec-av1's,
//! deliberately, so one bound covers the by-value class across the codec
//! crates rather than each crate calibrating its own.
//!
//! **The quantity being bounded.** Not `size_of` in the abstract, but
//! *what a caller can be made to owe*: the size of one by-value return,
//! multiplied by how many public levels hold a copy at once. Three inputs:
//! 1. **The smallest stack a caller can hand this crate is 2 MiB.** That
//!    is libtest's default test thread and what `std::thread::Builder`
//!    hands a caller who does not ask for a size. A library consumer gets
//!    no `.cargo/config.toml` -- and this repo's sets
//!    `RUST_MIN_STACK = 67108864`, which HIDES the problem from this
//!    workspace's own runs. The bound is therefore derived against the
//!    2 MiB an unconfigured caller really has.
//! 2. **The deepest PUBLIC by-value chain is 3**
//!    ([`DEEPEST_PUBLIC_CHAIN`]): `stream::decode_stream` (stream.rs:74)
//!    -> `stream::decode_stream_with` (stream.rs:88) ->
//!    `Decoder::decode` (decode.rs:238), after which the remaining frames
//!    (`decode_one`, `decode_keyframe`, `decode_inter`) are private. The
//!    test asserts `DEEPEST_PUBLIC_CHAIN * worst * 8 <= 2 MiB` -- the
//!    by-value return slots may claim at most an EIGHTH of a caller's
//!    stack, leaving the rest for the decode walk, which is the thing that
//!    actually needs it.
//! 3. **The budget applies only to types a `pub fn` returns.** The
//!    crate-private 2,039-byte `FrameContext` is measured and listed but
//!    NOT bounded, because no caller can reach it by value -- every field
//!    holding one is a `Box`, pinned by
//!    [`the_frame_contexts_stay_behind_a_box`]. Bounding it would
//!    calibrate the number to something unreachable.
//!
//! **Where the number sits.** At 8 KiB with the worst public return at
//! 768 bytes ([`Decoder`]), the worst a caller owes on return slots
//! alone is `3 x 768 = 2,304` bytes -- 0.11% of a 2 MiB thread, and the
//! budget itself is 10.7x the largest live by-value type. Both directions
//! have room, which is the point: a bound the largest type sits just
//! under is a bound the next lane will quietly raise.
//!
//! **What it catches.** The pre-fix `Decoder` (10,944 B) is **1.34x over**
//! -- the SMALLEST over-budget case of the three crates this sweep has
//! measured (ec-av1's pre-fix `Av1Encoder` was 17.1x over, ec-ac3's
//! pre-fix `Ac3Decoder` 2.43x). It still reds, and by a factor rather than
//! a hair, which is all a bound has to do.
//!
//! # This crate's inventory is SMALL, and that is a measurement
//!
//! ec-av1's floor for bound-checked public rows is 10. **ec-vp9 genuinely
//! has 3** ([`Decoder`], [`Picture`], [`BoolDecoder`]) and the floor is
//! lowered to 3 rather than padded with types this crate does not have.
//! The reason is structural, not accidental: every other module here
//! (`inter`, `mc`, `modes`, `tokens`, `transform`, `loopfilter`, `intra`,
//! `header`, `tables`) exposes ZERO `pub fn` -- its API is
//! `pub(crate)`, reached through `Decoder`. There are 13 `pub fn` lines
//! in the whole crate, 11 of which return a primitive, a `Self`, or
//! `()`.
//!
//! The sweep is still worth having at that size, and the reason is
//! direction of travel: the scan found exactly ONE by-value public return
//! name (`Picture`, from `Decoder::decode`), because `Decoder::new`
//! returns `Self` and `BoolDecoder::new` returns `Result<Self>`, both of
//! which the matcher exempts. A tiny inventory is a floor to grow into,
//! and [`every_by_value_public_return_is_in_the_inventory`] is what makes
//! a new 40 KB by-value return impossible to add unnoticed -- which is the
//! only direction that can actually bite here.
//!
//! # What it does not claim
//!
//! `size_of` is layout, not stack depth. The residual is why
//! [`the_deepest_by_value_constructors_fit_on_a_tight_stack`] constructs
//! for real instead of trusting the arithmetic. And note
//! [`the_frame_contexts_stay_behind_a_box`] pins ONE field: `Decoder::ctx`
//! (decode.rs:129) is still an INLINE `FrameContext`, and at 2,039 bytes
//! it is 72.8% of the 2,800 a caller pays today. Boxing that too is the
//! obvious next move in this class; this module only records the
//! measurement.

use crate::bool::BoolDecoder;
use crate::decode::{Decoder, Picture};

/// The per-type ceiling for a type a CALLER can be made to hold by value, in
/// bytes. See the module docs for the derivation; it is two-sided, and both
/// sides are measured.
pub(crate) const STACK_BUDGET: usize = 8192;

/// The deepest PUBLIC by-value chain in this crate, counted from the source:
/// `stream::decode_stream` (stream.rs:74) -> `stream::decode_stream_with`
/// (stream.rs:88) -> `Decoder::decode` (decode.rs:238) is three public
/// levels, after which `decode_one` / `decode_keyframe` / `decode_inter`
/// are private. The budget is checked against three.
///
/// The by-value return slots IN that chain are all small
/// (`Result<Vec<Picture>>`, `Result<()>`, `Result<Option<Picture>>`); the
/// 2,800-byte `Decoder` is a by-value LOCAL in `decode_stream_with` that
/// stays live across the inner `decode` calls. Multiplying the chain by the
/// crate's worst public return is the conservative direction, and is what
/// [`every_by_value_type_fits_the_stack_budget`] asserts.
#[cfg(test)]
const DEEPEST_PUBLIC_CHAIN: usize = 3;

/// Every type in this crate a public `fn` can return BY VALUE, with the
/// `size_of` measured at the commit that wrote this table. `None` means "not
/// yet measured", which [`the_measured_inventory_is_accurate`] rejects.
///
/// The third column names the return path, so a reader can see why a type is
/// here rather than a coincidence of the type system.
pub(crate) const BY_VALUE_TYPES: &[(&str, Option<usize>, bool, &str)] = &[
    // -- decode.rs --------------------------------------------------------
    (
        "Decoder",
        Some(768),
        true,
        "Decoder::new -> Self (and `Default::default`), the crate's whole \
         public surface. Was 10944 with five inline FrameContext slots \
         (10,195 of it), 1.34x over budget; frame_ctxs and ctx are both \
         Boxes now. Boxing frame_ctxs ALONE left 2800 -- under budget, but \
         with ctx still inline that was 72.8% of it",
    ),
    (
        "Picture",
        Some(96),
        true,
        "Decoder::decode -> Result<Option<Picture>>. Its fields ARE pub \
         (y/u/v are Vec<Sample>), but it is thin -- three Vecs are three \
         heap pointers, so the 96 bytes are a stack cost of three words, \
         not of frame data",
    ),
    // -- bool.rs ----------------------------------------------------------
    (
        "BoolDecoder",
        Some(40),
        true,
        "bool::BoolDecoder::new(data) -> Result<Self>; the sole public \
         by-value return in the crate besides Picture, and the only type a \
         caller can hold by value outside the decode surface",
    ),
    // -- crate-private: never a public return, and the one that was
    // -- inline is behind a Box now. The budget does not apply; the pin
    // -- that does is `the_frame_contexts_stay_behind_a_box`.
    (
        "FrameContext",
        Some(2039),
        false,
        "header::FrameContext is pub(crate) and the payload of BOTH \
         `frame_ctxs: Box<[FrameContext; 4]>` and `ctx: Box<FrameContext>`; \
         five inline were 10195 of the pre-fix 10944. No FrameContext is \
         stored inline in Decoder any more",
    ),
];

/// The live `size_of` for every inventoried type, keyed BY NAME.
///
/// Keyed rather than positional on purpose: a row inserted into
/// [`BY_VALUE_TYPES`] without a matching insert here would zip cleanly and
/// assert the wrong type's size against the wrong recorded one. A name key
/// makes that mistake loud instead.
#[cfg(test)]
fn measured_by_name() -> Vec<(&'static str, usize)> {
    vec![
        ("Decoder", std::mem::size_of::<Decoder>()),
        ("Picture", std::mem::size_of::<Picture>()),
        ("BoolDecoder", std::mem::size_of::<BoolDecoder<'static>>()),
        (
            "FrameContext",
            std::mem::size_of::<crate::header::FrameContext>(),
        ),
    ]
}

/// The stack [`the_deepest_by_value_constructors_fit_on_a_tight_stack`]
/// constructs on.
///
/// 256 KiB: an eighth of what an unconfigured caller hands this crate, and
/// far above what a 2,800-byte by-value `Decoder` needs in a debug build
/// (the threshold is swept with `EC_VP9_TIGHT_STACK_BYTES`).
const TIGHT_STACK: usize = 256 * 1024;

#[cfg(test)]
mod tests {
    use super::{
        BY_VALUE_TYPES, DEEPEST_PUBLIC_CHAIN, STACK_BUDGET, TIGHT_STACK, measured_by_name,
    };
    use crate::bool::BoolDecoder;
    use crate::decode::Decoder;

    /// Return types that are not a by-value copy of anything the caller has
    /// to own: a pointer, a slice, a `Vec` (three words, and the heap is
    /// not the stack), or a primitive. `Self` is a by-value copy of the
    /// impl's type, which the inventory rows already cover, so it is exempt
    /// from the scan and not from the table.
    const THIN: &[&str] = &[
        "Box", "Rc", "Arc", "Cow", "Vec", "VecDeque", "String", "OsString", "PathBuf", "HashMap",
        "BTreeMap", "BTreeSet", "HashSet", "str", "u8", "u16", "u32", "u64", "usize", "i8", "i16",
        "i32", "i64", "isize", "f32", "f64", "bool", "char", "Self",
    ];

    /// The type name a `pub fn` line returns BY VALUE, or `None` when the
    /// line returns a pointer, a `Vec`, a primitive, or nothing.
    ///
    /// One function, shared by the sweep and its control, so the control
    /// cannot pass while the sweep's copy of the logic rotted.
    fn by_value_return_name(line: &str) -> Option<String> {
        let trimmed = line.trim_start();
        if !trimmed.starts_with("pub fn ") {
            return None;
        }
        let arrow = line.find("->")?;
        let rest = &line[arrow + 2..];
        let end = rest.find('{').unwrap_or(rest.len());
        let ret = rest[..end].trim();
        if ret.is_empty() {
            return None;
        }
        // Unwrap the `Result<..>` / `Option<..>` layers. ec-av1 unwraps one;
        // this crate needs the loop, because its ONE by-value public return
        // is spelled `Result<Option<Picture>>` (decode.rs:238) and a
        // single unwrap reads that as the name "Option" -- an uninventoried
        // non-type that would have made the sweep permanently red for a
        // reason that has nothing to do with the class.
        let mut inner = ret;
        loop {
            let stripped = inner
                .strip_prefix("Result<")
                .or_else(|| inner.strip_prefix("Option<"));
            match stripped {
                Some(t) => {
                    let head = t.split('>').next().unwrap_or(t);
                    if head.is_empty() {
                        return None;
                    }
                    inner = head;
                }
                None => break,
            }
        }
        // Drop the error type and any lifetime or generic argument:
        // `Result<Decoder, Error>`, `BoolDecoder<'a>` and `Picture` are all
        // the bare name for this purpose. Splitting on the comma is
        // load-bearing: the first version of this matcher cut only at `>`
        // and so read `Result<Decoder, Error>` as the name
        // "Decoder, Error".
        let name = inner.split(['<', ',']).next().unwrap_or(inner).trim();
        if name.is_empty() || THIN.contains(&name) {
            return None;
        }
        // A generic parameter, not a concrete type.
        if !name.chars().next().is_some_and(char::is_uppercase) {
            return None;
        }
        Some(name.to_string())
    }

    /// The source files the by-value scan reads. Every module `lib.rs`
    /// declares, plus this one; a new module added to `lib.rs` needs
    /// adding here, and [`every_by_value_module_is_scanned`] is what keeps
    /// that honest.
    ///
    /// The label is the path relative to `src/`, so a directory module is
    /// `tables/mod.rs` rather than `tables.rs`.
    const FILES: &[(&str, &str)] = &[
        ("bool.rs", include_str!("bool.rs")),
        ("decode.rs", include_str!("decode.rs")),
        ("header.rs", include_str!("header.rs")),
        ("inter.rs", include_str!("inter.rs")),
        ("intra.rs", include_str!("intra.rs")),
        ("loopfilter.rs", include_str!("loopfilter.rs")),
        ("mc.rs", include_str!("mc.rs")),
        ("modes.rs", include_str!("modes.rs")),
        ("stream.rs", include_str!("stream.rs")),
        ("tables/mod.rs", include_str!("tables/mod.rs")),
        ("tokens.rs", include_str!("tokens.rs")),
        ("transform.rs", include_str!("transform.rs")),
        ("stack_budget.rs", include_str!("stack_budget.rs")),
    ];

    /// Every module `lib.rs` declares, so the scan's file list cannot
    /// silently fall behind a new module. A new `mod foo;` with a
    /// by-value `pub fn` that the scan never reads is exactly the hole
    /// this class would come back through, so the list is derived, not
    /// curated.
    #[test]
    fn every_by_value_module_is_scanned() {
        let lib = include_str!("lib.rs");
        let mut declared: Vec<&str> = Vec::new();
        for line in lib.lines() {
            let t = line.trim();
            let rest = t
                .strip_prefix("pub mod ")
                // This crate's `inter` and `mc` are `pub(crate) mod`, a
                // spelling ec-av1 has no instance of. They are declared
                // modules and are scanned, so they must be derived here
                // too -- otherwise the check below is blind to exactly
                // the two files a future by-value return would most
                // likely land in.
                .or_else(|| t.strip_prefix("pub(crate) mod "))
                .or_else(|| t.strip_prefix("mod "))
                .unwrap_or("");
            if rest.is_empty() {
                continue;
            }
            let name = rest.split([';', ' ']).next().unwrap_or("");
            if !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                declared.push(name);
            }
        }
        // 12 modules (incl. the `pub(crate)` ones -- `inter`, `mc`) plus
        // this one. The floor is a REACH floor: below it the parse is
        // broken, not the crate small.
        assert!(
            declared.len() >= 13,
            "only {} module declarations parsed out of lib.rs, so the derived \
             file list is broken rather than small",
            declared.len()
        );
        let scanned: Vec<&str> = FILES.iter().map(|(n, _)| *n).collect();
        let mut missing: Vec<String> = declared
            .iter()
            .filter(|d| {
                let flat = format!("{d}.rs");
                let dir = format!("{d}/mod.rs");
                !scanned.iter().any(|s| *s == flat || *s == dir)
            })
            .map(|d| (*d).to_string())
            .collect();
        missing.sort();
        assert!(
            missing.is_empty(),
            "lib.rs declares modules the by-value scan never reads: {:?}. Add \
             them to FILES, or the sweep cannot see a new by-value return that \
             lands in one of them.",
            missing
        );
    }

    /// The inventory, printed as a sorted table, and checked against the
    /// types' real sizes.
    ///
    /// The print is the instrument: one command
    /// (`cargo test -p ec-vp9 --lib -- stack_budget -- --nocapture`)
    /// reproduces the whole table, biggest first, so "what is the largest
    /// by-value type in this crate" has an answer that is a measurement
    /// rather than a grep.
    ///
    /// The check reds on ANY change with both numbers. That is deliberate: a
    /// by-value type that grew raises the question of whether it should
    /// have been boxed instead, and auto-accepting the new number would
    /// answer that question by default.
    #[test]
    fn the_measured_inventory_is_accurate() {
        let live = measured_by_name();
        assert_eq!(
            BY_VALUE_TYPES.len(),
            live.len(),
            "the inventory names {} types but {} are measured; every row needs \
             a measured size or the sweep is a list of hopes",
            BY_VALUE_TYPES.len(),
            live.len()
        );
        let mut rows: Vec<(&str, usize)> = live.clone();
        rows.sort_by_key(|&(_, size)| std::cmp::Reverse(size));
        eprintln!(
            "\nBY-VALUE INVENTORY ({} types, budget {STACK_BUDGET} B = {} KiB)",
            rows.len(),
            STACK_BUDGET / 1024
        );
        eprintln!("{:>8}  {:>7}  {}", "BYTES", "%BUDGET", "TYPE");
        for (name, size) in &rows {
            eprintln!(
                "{size:>8}  {:>6.1}%  {name}",
                *size as f64 * 100.0 / STACK_BUDGET as f64
            );
        }
        for (name, recorded, _public, _path) in BY_VALUE_TYPES {
            let recorded = recorded.expect("every inventory row carries a measured size");
            let actual = live
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, size)| *size)
                .unwrap_or_else(|| panic!("{name} is inventoried but never measured"));
            assert_eq!(
                recorded, actual,
                "{name} is {actual} bytes, the inventory records {recorded}; \
                 update the table deliberately -- a by-value type that changed \
                 is a stack cost every caller of its constructor now pays"
            );
        }
    }

    /// The bound, asserted BY NAME. A regression in any inventoried type
    /// reds here naming the type, not "some type is too big".
    #[test]
    fn every_by_value_type_fits_the_stack_budget() {
        let live = measured_by_name();
        let mut checked = 0usize;
        for (name, _, public, path) in BY_VALUE_TYPES {
            if !public {
                continue;
            }
            checked += 1;
            let size = live
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, size)| *size)
                .unwrap_or_else(|| panic!("{name} is inventoried but never measured"));
            assert!(
                size <= STACK_BUDGET,
                "{name} is {size} bytes, over the {STACK_BUDGET}-byte by-value \
                 stack budget ({:.2}x). It is returned by value from {path}, so \
                 every caller pays it on its own stack. Box the large field \
                 (frame_ctxs: Box<[FrameContext; 4]> did exactly this for \
                 Decoder, 10944 -> 2800) or hand back a reference.",
                size as f64 / STACK_BUDGET as f64
            );
        }
        // ec-av1's floor is 10. THIS CRATE HAS 3, and that is a measurement,
        // not a gap to pad: every module but `bool`, `decode` and `stream`
        // exposes zero `pub fn` (its API is `pub(crate)`, reached through
        // `Decoder`). Inventing rows to reach ec-av1's number would make
        // the gate theatre -- it would count types no caller can reach.
        assert!(
            checked == 3,
            "{checked} public rows were bound-checked, and this crate has \
             exactly 3 types a caller can hold by value (Decoder, Picture, \
             BoolDecoder). If a fourth appeared, measure it, add it to \
             BY_VALUE_TYPES, and raise this floor with it; if a row was \
             removed, the count is wrong here and in the inventory."
        );
        // The other half of the derivation, asserted so it cannot rot: the
        // worst a caller can owe is DEEPEST_CHAIN x the worst public return.
        let worst = live
            .iter()
            .filter(|(n, _)| {
                BY_VALUE_TYPES
                    .iter()
                    .any(|(row, _, public, _)| row == n && *public)
            })
            .map(|(_, size)| *size)
            .max()
            .expect("at least one public row");
        assert!(
            DEEPEST_PUBLIC_CHAIN * worst * 8 <= 2 * 1024 * 1024,
            "the deepest public chain ({DEEPEST_PUBLIC_CHAIN}) times the worst \
             public by-value return ({worst} B) is {} B, over an eighth of the \
             2 MiB stack an unconfigured caller hands this crate; \
             re-derive STACK_BUDGET",
            DEEPEST_PUBLIC_CHAIN * worst
        );
    }

    /// The four stored frame contexts stay BEHIND A BOX.
    ///
    /// The budget does not apply to `header::FrameContext` (2,039 B) because
    /// no `pub fn` returns it, so bounding it would calibrate the number to
    /// something no caller can reach. What keeps four of them off a
    /// caller's stack is that the array is a `Box`, and THIS is the pin.
    ///
    /// It earned its place twice. `Decoder` was 10,944 B, of which 8,156 was
    /// `frame_ctxs: [FrameContext; 4]` sitting inline -- 1.34x over budget.
    /// Boxing the array alone left 2,800, of which `ctx` was 2,039 (72.8%):
    /// under the budget, but with a single field carrying nearly all of it.
    #[test]
    fn the_frame_contexts_stay_behind_a_box() {
        const PINS: &[(&str, &str)] = &[
            // The field declarations (decode.rs:134, decode.rs:139).
            ("frame_ctxs: Box<[FrameContext; 4]>", "the frame_ctxs field"),
            ("ctx: Box<FrameContext>", "the ctx field"),
            // Their construction in `Decoder::new` (decode.rs:216, 221).
            (
                "Box::new([d.clone(), d.clone(), d.clone(), d])",
                "Decoder::new",
            ),
            ("ctx: Box::new(FrameContext::new(true))", "Decoder::new"),
        ];
        const SRC: &str = include_str!("decode.rs");
        for (spelling, where_) in PINS {
            assert!(
                SRC.contains(spelling),
                "decode.rs::{where_} is no longer declared `{spelling}`. An \
                 inline `FrameContext` there puts 2039 bytes back into every \
                 caller's stack -- five of them were 10195 bytes, which is how \
                 `Decoder` was 10944 bytes and 1.34x over the by-value budget \
                 before the boxes.",
            );
        }
        // And the negative control: the unboxed spelling must NOT still be
        // present, or the scan above would be matching a comment.
        assert!(
            !SRC.contains("frame_ctxs: [FrameContext; 4],") && !SRC.contains("ctx: FrameContext,"),
            "decode.rs still spells a FrameContext field inline, so the box \
             pin above is matching a spelling that is not the live declaration"
        );
    }

    /// CAPABILITY: really construct the heavy type on a tight stack, so a
    /// regression ABORTS THE PROCESS instead of costing invisible margin.
    ///
    /// `Decoder::new` is the crate's one large by-value return path, and it
    /// is also the one whose boxing moved the threshold.
    ///
    /// **What this arm does and does not catch, measured.** Swept in an
    /// unoptimized build, one process per point:
    ///
    /// | stack | pre-fix (unboxed, 10,944 B) | boxed (2,800 B) |
    /// |---|---|---|
    /// | 16384 | overflow | overflow |
    /// | 24576 | overflow | overflow |
    /// | 32768 | overflow | **ok** |
    /// | 40960 | **ok** | ok |
    /// | 262144 (the gate) | **ok** | ok |
    ///
    /// So the boxing moved the threshold from 40,960 to 32,768 -- real, and
    /// HONESTLY SMALL: a 10,944-byte struct does not need a small stack to
    /// overflow. **The unboxing mutation does NOT red this arm at 256 KiB.**
    /// It reds [`every_by_value_type_fits_the_stack_budget`] (named, with
    /// the 1.34x factor) and [`the_frame_contexts_stay_behind_a_box`]
    /// instead. This arm is the capability floor -- it pins the real live
    /// stack of the crate's by-value return path, which the arithmetic in
    /// the other two tests does not model -- and it is deliberately set
    /// well clear of the measured 32,768 B so an unoptimised-build
    /// regression in stack need shows up here as a margin loss rather than
    /// as a mysterious abort. Reproduce either column with
    /// `EC_VP9_TIGHT_STACK_BYTES`.
    ///
    /// The env read is a raw `std::env::var`, not the crate's `cached_gate!`
    /// macro: `cached_gate!` answers PRESENCE and caches a bool, and this
    /// override needs a VALUE, read once per test run and never on a decode
    /// path. Using the hot-path convention for a cold, value-shaped read
    /// would be the wrong tool.
    #[test]
    fn the_deepest_by_value_constructors_fit_on_a_tight_stack() {
        let stack: usize = std::env::var("EC_VP9_TIGHT_STACK_BYTES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(TIGHT_STACK);
        let handle = std::thread::Builder::new()
            .stack_size(stack)
            .spawn(|| {
                // The by-value return path: a whole `Decoder` built and
                // returned from `new` on a 256 KiB stack.
                let decoder = Decoder::new();
                // Real post-construction state, not just "did not crash": a
                // fresh decoder has parsed no inter frame and produced no
                // blocks, and the four boxed frame contexts are addressable
                // and distinct from the live `ctx`.
                let blocks = decoder.last_frame_blocks();
                let bools = BoolDecoder::new(&[]).expect("BoolDecoder::new on an empty partition");
                let overreads = bools.overreads();
                (blocks, overreads, std::mem::size_of::<Decoder>())
            })
            .expect("spawn tight-stack thread");
        let (blocks, overreads, size) = handle.join().unwrap_or_else(|_| {
            panic!(
                "Decoder::new overflowed a {stack}-byte stack; that is what the \
                 pre-fix 10944-byte by-value layout costs a caller"
            )
        });
        assert_eq!(
            blocks, 0,
            "a freshly constructed Decoder reports {blocks} last-frame blocks"
        );
        assert_eq!(
            overreads, 0,
            "an empty BoolDecoder partition reports {overreads} overreads"
        );
        eprintln!("TIGHTSTACK {stack} bytes: Decoder={size} B, constructed and returned");
    }

    /// The sweep's teeth against a NEW by-value return: re-derive the set
    /// from the crate source and require every name to be inventoried.
    ///
    /// Without this the inventory is a list of what someone remembered to
    /// measure, and a lane that adds `pub fn make() -> HugeThing` where
    /// `HugeThing` is 200 KB of inline tables passes every other test here.
    #[test]
    fn every_by_value_public_return_is_in_the_inventory() {
        let mut found: Vec<(String, String)> = Vec::new();
        let mut pub_fn_lines = 0usize;
        for (file, src) in FILES {
            for (n, line) in src.lines().enumerate() {
                if line.trim_start().starts_with("pub fn ") {
                    pub_fn_lines += 1;
                }
                if let Some(name) = by_value_return_name(line) {
                    found.push((name, format!("{file}:{}", n + 1)));
                }
            }
        }
        found.sort();
        found.dedup();
        // A REACH floor, not a size floor: ec-av1 asserts > 5 names because
        // it has 20 KB of encoders. This crate has 13 `pub fn` lines in
        // total, 11 of which return a primitive / `Self` / `()`, so the
        // one by-value public return it really has is `Picture`. Asserting
        // a bigger number would be asserting a fact about ec-av1.
        assert!(
            pub_fn_lines >= 10,
            "the scan saw only {pub_fn_lines} `pub fn` lines across {} files, \
             too few to be a real sweep -- the matcher, not the crate, is \
             broken",
            FILES.len()
        );
        assert!(
            !found.is_empty(),
            "the by-value return scan found NO name across {} files and {} \
             `pub fn` lines; `Decoder::decode -> Result<Option<Picture>>` \
             (decode.rs:238) must be in there, so the matcher is broken",
            FILES.len(),
            pub_fn_lines
        );

        let mut missing = Vec::new();
        for (name, site) in &found {
            if !BY_VALUE_TYPES.iter().any(|(n, _, _, _)| n == name) {
                missing.push(format!("{name} ({site})"));
            }
        }
        assert!(
            missing.is_empty(),
            "these types are returned BY VALUE from a public fn and are not in \
             the inventory: {}. Either they are thin enough not to be a stack \
             cost -- say so by adding a row with its measured size -- or they \
             are the next instance of the class this module exists for.",
            missing.join(", ")
        );
        eprintln!(
            "BYVALUE-SCAN {} `pub fn` lines, {} by-value public return name(s): {:?}",
            pub_fn_lines,
            found.len(),
            found
        );
    }

    /// The control for the scan above, on synthetic text rather than by
    /// trusting the live tree to happen to contain a case.
    ///
    /// If the matcher silently stopped recognising a shape, the sweep would
    /// pass everything and this would still be asserting nothing. So: every
    /// by-value spelling this crate uses must yield its type name, and every
    /// pointer/Vec/primitive/`Self` spelling must yield nothing.
    #[test]
    fn the_inventory_scan_is_not_vacuous() {
        for synthetic in [
            "pub fn zz_a() -> ZzNotInventoried { todo!() }",
            "pub fn zz_b() -> Result<ZzNotInventoried, Error> { todo!() }",
            "pub fn zz_c() -> Result<ZzNotInventoried> { todo!() }",
            "pub fn zz_d() -> Option<ZzNotInventoried> { todo!() }",
            "pub fn zz_e() -> ZzNotInventoried<'a> { todo!() }",
            // The DOUBLE-wrapped shape this crate actually uses, at
            // decode.rs:238. A single-layer unwrap reads it as "Option",
            // which is not a type and would red the sweep forever.
            "pub fn zz_f() -> Result<Option<ZzNotInventoried>> { todo!() }",
        ] {
            let name = by_value_return_name(synthetic)
                .unwrap_or_else(|| panic!("the matcher does not recognise `{synthetic}`"));
            assert_eq!(
                name, "ZzNotInventoried",
                "the matcher mangled `{synthetic}` into `{name}`, so the sweep \
                 cannot see a new by-value return and its green is vacuous"
            );
            assert!(
                !BY_VALUE_TYPES.iter().any(|(n, _, _, _)| *n == name),
                "the synthetic control name is in the inventory, so the control \
                 proves nothing"
            );
        }
        for thin in [
            "pub fn zz_t() -> Box<Decoder> { todo!() }",
            "pub fn zz_u() -> Result<Vec<u8>, Error> { todo!() }",
            "pub fn zz_v() -> Option<&Path> { todo!() }",
            "pub fn zz_w() -> usize { todo!() }",
            "pub fn zz_x() -> Self { todo!() }",
            "pub fn zz_y() -> Result<Arc<Decoder>, Error> { todo!() }",
            "pub fn zz_z() { todo!() }",
            "fn zz_private() -> ZzNotInventoried { todo!() }",
            // The two live spellings this crate exempts on purpose:
            // `Vec` is three words and its heap is not the caller's stack.
            "pub fn zz_g() -> Result<Vec<Picture>> { todo!() }",
            "pub fn zz_h() -> Result<Self> { todo!() }",
        ] {
            assert_eq!(
                by_value_return_name(thin),
                None,
                "`{thin}` is a pointer, a Vec, a primitive, a `Self` the rows \
                 above already cover, or not a `pub fn` -- the matcher must not \
                 read it as an uninventoried by-value return"
            );
        }
    }
}
