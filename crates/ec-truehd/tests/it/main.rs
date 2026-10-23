//! One integration-test binary for ec-truehd; each module is one former `tests/*.rs` file.
#![forbid(unsafe_code)]

mod atmos_synth;
mod common;
mod fixture_exact;
mod fuzz_regressions;
mod library;
mod oracle;
mod refused_au_recovery;
