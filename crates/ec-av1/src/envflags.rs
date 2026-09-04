//! Process-constant snapshot of the environment (lane-perf1).
//!
//! Every `EC_*` debug/trace flag in this crate is read on per-block, per-TU or
//! even per-MC-call paths, and `std::env::var*` is a `getenv` + `strlen` +
//! `CStr` validation every time: 11.6% of `decode_probe`'s self time. Nothing
//! in the crate or its tests calls `set_var` (`grep -rn set_var crates/ec-av1`
//! = 0 hits), so each name is a process constant and may be read once.
//!
//! [`env_flag!`] is the zero-syscall form for `is_some()`/`is_none()` checks:
//! it keeps one `LazyLock<bool>` per CALL SITE, so a hot check compiles to an
//! atomic load. [`var`] keeps the `Result<String, VarError>` shape of
//! `std::env::var` for the sites that want the value, off one snapshot map.

use std::collections::HashMap;
use std::env::VarError;
use std::sync::LazyLock;

static ENV: LazyLock<HashMap<String, String>> = LazyLock::new(|| std::env::vars().collect());

/// `std::env::var`'s contract, served from the snapshot. Non-UTF-8 values are
/// dropped by `std::env::vars()`; the crate only reads `EC_*` flags and paths.
pub(crate) fn var(name: &str) -> Result<String, VarError> {
    ENV.get(name).cloned().ok_or(VarError::NotPresent)
}

/// `true` iff `name` is present in the environment. Prefer [`env_flag!`] on
/// hot paths -- this one still hashes the name per call.
pub(crate) fn is_set(name: &str) -> bool {
    ENV.contains_key(name)
}

/// One `LazyLock<bool>` per call site: `std::env::var_os(N).is_some()` with no
/// syscall and no hashing after the first call.
macro_rules! env_flag {
    ($name:literal) => {{
        static FLAG: std::sync::LazyLock<bool> =
            std::sync::LazyLock::new(|| $crate::envflags::is_set($name));
        *FLAG
    }};
}
pub(crate) use env_flag;

#[cfg(test)]
mod tests {
    #[test]
    fn snapshot_matches_process_env() {
        // PATH is set for every test process; a name no one exports is absent.
        assert_eq!(super::is_set("PATH"), std::env::var_os("PATH").is_some());
        assert_eq!(super::var("PATH").ok(), std::env::var("PATH").ok());
        assert!(!super::is_set("EC_AV1_ENVFLAGS_NEVER_SET"));
        assert!(!env_flag!("EC_AV1_ENVFLAGS_NEVER_SET"));
    }
}
