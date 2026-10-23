use super::*;
use super::vectors::VECTORS;

#[test]
fn matches_reference_derivation_on_500_configurations() {
    let mut mismatches = 0;
    for &(
        rate,
        start_freq,
        stop_freq,
        freq_scale,
        alter_scale,
        noise_bands,
        xover_band,
        expected,
    ) in VECTORS.iter().flat_map(|part| part.iter())
    {
        let got = freq_tables(
            rate,
            start_freq,
            stop_freq,
            freq_scale,
            alter_scale,
            xover_band,
            noise_bands,
        );
        let ok = match (&got, expected) {
            (None, None) => true,
            (Some(t), Some((n_master, n_high, n_low, n_q, f_high, f_low, f_noise))) => {
                t.n_master == n_master
                    && t.n_high == n_high
                    && t.n_low == n_low
                    && t.n_q == n_q
                    && t.f_high == f_high
                    && t.f_low == f_low
                    && t.f_noise == f_noise
            }
            _ => false,
        };
        if !ok {
            mismatches += 1;
            if mismatches <= 10 {
                eprintln!(
                    "mismatch rate={rate} start={start_freq} stop={stop_freq} scale={freq_scale} \
                     alter={alter_scale} noise={noise_bands} xover={xover_band}: got {got:?} want {expected:?}"
                );
            }
        }
    }
    assert_eq!(
        mismatches, 0,
        "{mismatches}/500 configurations disagreed with the reference derivation"
    );
}

#[test]
fn bands_are_monotonic_when_present() {
    for &(
        rate,
        start_freq,
        stop_freq,
        freq_scale,
        alter_scale,
        noise_bands,
        xover_band,
        expected,
    ) in VECTORS.iter().flat_map(|part| part.iter())
    {
        if expected.is_none() {
            continue;
        }
        let t = freq_tables(
            rate,
            start_freq,
            stop_freq,
            freq_scale,
            alter_scale,
            xover_band,
            noise_bands,
        )
        .expect("vector says Some");
        assert!(t.f_high.windows(2).all(|w| w[0] < w[1]));
        assert!(t.f_low.windows(2).all(|w| w[0] < w[1]));
        assert!(t.f_noise.windows(2).all(|w| w[0] < w[1]));
    }
}
