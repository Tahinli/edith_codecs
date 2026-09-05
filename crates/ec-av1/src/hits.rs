//! Gate-counter bumps, compiled out when the `gate-counters` feature is off.
//!
//! The ~550 `*_HITS` thread-locals in this crate exist only so the test gates
//! (and the probe's census print) can prove a coding path fired; every bump is
//! a TLS access plus a `Cell` read-modify-write on the parse thread. Routing
//! all of them through [`hit!`]/[`hit_do!`] lets a shipped decoder drop them
//! (`--no-default-features`) while the accessors keep compiling and returning
//! whatever the counters hold (0 when the feature is off), so no caller needs
//! a `cfg`.

/// Bumps counter `$name` by one -- nothing at all without `gate-counters`.
/// Usable in expression position (it evaluates to `()`).
#[cfg(feature = "gate-counters")]
macro_rules! hit {
    ($name:ident) => {{
        $name.with(|c| c.set(c.get() + 1));
    }};
}

#[cfg(not(feature = "gate-counters"))]
macro_rules! hit {
    ($name:ident) => {{}};
}

/// Same, for a bump that is not a plain `+= 1` (array/tuple buckets, the
/// `inter_last_mc` stamp, the counter-only scan loops): the whole statement
/// list disappears without `gate-counters`.
#[cfg(feature = "gate-counters")]
macro_rules! hit_do {
    ($($t:tt)*) => {{
        $($t)*
    }};
}

// Without the feature the statements stay type-checked but unreachable, so the
// values they read (bucket indices, filter kinds) still count as used and the
// optimiser drops the whole branch.
#[cfg(not(feature = "gate-counters"))]
macro_rules! hit_do {
    ($($t:tt)*) => {{
        if false {
            $($t)*
        }
    }};
}

pub(crate) use {hit, hit_do};
