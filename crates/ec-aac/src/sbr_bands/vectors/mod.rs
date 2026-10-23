//! Reference vectors for the `freq_tables` tests: 500 configurations sampled from the
//! same `freq_tables()` Python function the project ledger records as checked against a
//! reference decoder over 6387 configurations; regenerate with
//! `scripts/aac-tables/sbrtables.py`'s `freq_tables` if that function ever changes.

mod part0;
mod part1;

/// `(sbr_rate, start_freq, stop_freq, freq_scale, alter_scale, noise_bands,
/// xover_band, expected)`, `expected` being `(n_master, n_high, n_low, n_q,
/// f_high, f_low, f_noise)`.
#[allow(clippy::type_complexity)]
pub(super) type Vector = (
    u32,
    usize,
    usize,
    i32,
    i32,
    i32,
    usize,
    Option<(usize, usize, usize, usize, &'static [i64], &'static [i64], &'static [i64])>,
);

/// All reference vectors, in two parts.
pub(super) static VECTORS: &[&[Vector]] = &[part0::PART, part1::PART];
