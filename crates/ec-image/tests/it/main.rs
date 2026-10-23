//! ec-image integration tests, one binary. Each module is one former `tests/*.rs` file
//! (`fuzz` and `fuzz_regressions` stay separate binaries; see their headers).
#![forbid(unsafe_code)]

mod differential;
mod gaps;
mod real_library;
