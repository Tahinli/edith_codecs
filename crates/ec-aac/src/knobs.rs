//! Debug switches the AAC/SBR decoder reads from the environment, with a
//! per-thread override that tests set instead of editing the process
//! environment (which is `unsafe` in Rust 2024).
//!
//! The decoder reads these on the thread that calls `decode`, so an override
//! set on a test's own thread covers exactly that test's decodes and nothing
//! running in parallel. With no override set, each reader answers from the
//! environment exactly as it always has, so manual runs with `EC_AAC_*` set
//! behave as before.

use std::cell::Cell;
use std::thread::LocalKey;

type Slot = Cell<Option<bool>>;

thread_local! {
    static TOOL_SIDEINFO_DEBUG: Slot = const { Cell::new(None) };
    static SBR_SIDEINFO_DEBUG: Slot = const { Cell::new(None) };
    static SBR_HF_BYPASS: Slot = const { Cell::new(None) };
    static SBR_NOISE_ZERO: Slot = const { Cell::new(None) };
    static SBR_NOISE_FRACTION: Slot = const { Cell::new(None) };
}

/// The override on this thread if one is set, else whether `var` is set.
fn enabled(slot: &'static LocalKey<Slot>, var: &str) -> bool {
    slot.with(Cell::get)
        .unwrap_or_else(|| std::env::var(var).is_ok())
}

fn force(slot: &'static LocalKey<Slot>, on: Option<bool>) -> Override<Option<bool>> {
    let prev = slot.with(|c| c.replace(on));
    Override { slot, prev }
}

/// Restores the value a knob had before [`set_tool_sideinfo_debug`] and its
/// siblings were called, when dropped.
#[doc(hidden)]
#[must_use = "the override ends as soon as this guard is dropped"]
pub struct Override<T: Copy + 'static> {
    slot: &'static LocalKey<Cell<T>>,
    prev: T,
}

impl<T: Copy + 'static> Drop for Override<T> {
    fn drop(&mut self) {
        self.slot.with(|c| c.set(self.prev));
    }
}

/// `EC_AAC_TOOL_SIDEINFO_DEBUG`.
pub(crate) fn tool_sideinfo_debug() -> bool {
    enabled(&TOOL_SIDEINFO_DEBUG, "EC_AAC_TOOL_SIDEINFO_DEBUG")
}

/// `EC_AAC_SBR_SIDEINFO_DEBUG`.
pub(crate) fn sbr_sideinfo_debug() -> bool {
    enabled(&SBR_SIDEINFO_DEBUG, "EC_AAC_SBR_SIDEINFO_DEBUG")
}

/// `EC_AAC_SBR_HF_BYPASS`.
pub(crate) fn sbr_hf_bypass() -> bool {
    enabled(&SBR_HF_BYPASS, "EC_AAC_SBR_HF_BYPASS")
}

/// `EC_AAC_SBR_NOISE_ZERO`.
pub(crate) fn sbr_noise_zero() -> bool {
    enabled(&SBR_NOISE_ZERO, "EC_AAC_SBR_NOISE_ZERO")
}

/// `EC_AAC_SBR_NOISE_FRACTION`.
pub(crate) fn sbr_noise_fraction() -> bool {
    enabled(&SBR_NOISE_FRACTION, "EC_AAC_SBR_NOISE_FRACTION")
}

/// Forces `EC_AAC_TOOL_SIDEINFO_DEBUG` on (`Some(true)`), off (`Some(false)`)
/// or back to the environment (`None`) on this thread, until the guard drops.
#[doc(hidden)]
pub fn set_tool_sideinfo_debug(on: Option<bool>) -> Override<Option<bool>> {
    force(&TOOL_SIDEINFO_DEBUG, on)
}

/// [`set_tool_sideinfo_debug`] for `EC_AAC_SBR_SIDEINFO_DEBUG`.
#[doc(hidden)]
pub fn set_sbr_sideinfo_debug(on: Option<bool>) -> Override<Option<bool>> {
    force(&SBR_SIDEINFO_DEBUG, on)
}

/// [`set_tool_sideinfo_debug`] for `EC_AAC_SBR_HF_BYPASS`.
#[doc(hidden)]
pub fn set_sbr_hf_bypass(on: Option<bool>) -> Override<Option<bool>> {
    force(&SBR_HF_BYPASS, on)
}

/// [`set_tool_sideinfo_debug`] for `EC_AAC_SBR_NOISE_ZERO`.
#[doc(hidden)]
pub fn set_sbr_noise_zero(on: Option<bool>) -> Override<Option<bool>> {
    force(&SBR_NOISE_ZERO, on)
}

/// [`set_tool_sideinfo_debug`] for `EC_AAC_SBR_NOISE_FRACTION`.
#[doc(hidden)]
pub fn set_sbr_noise_fraction(on: Option<bool>) -> Override<Option<bool>> {
    force(&SBR_NOISE_FRACTION, on)
}
