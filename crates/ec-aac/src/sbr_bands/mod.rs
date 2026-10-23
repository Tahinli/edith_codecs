//! SBR frequency band tables (ISO/IEC 14496-3 §4.6.18.3.2.1): `f_master`,
//! `f_high`, `f_low`, `f_noise` from a header's `bs_start_freq`/`bs_stop_freq`/
//! `bs_freq_scale`/`bs_alter_scale`/`bs_xover_band`/`bs_noise_bands`.
//!
//! This is a direct port of `scripts/aac-tables/sbrtables.py`'s `freq_tables`/
//! `f_master`/`make_bands`, which was checked against a reference decoder over
//! 6387 header configurations (recorded in the project ledger). The port itself
//! is re-checked here against 500 configurations run through that same Python
//! function, embedded as literal test vectors so the check needs no Python at
//! test time -- only the numbers it once printed.
#![allow(dead_code)]

/// `k0` offset by SBR sample rate and `bs_start_freq`, ten rates the reference
/// decoder was probed at (`--stage k0`).
fn k0_offsets(rate: u32) -> Option<[i32; 16]> {
    Some(match rate {
        16000 => [-8, -7, -6, -5, -4, -3, -2, -1, 0, 1, 2, 3, 4, 5, 6, 7],
        22050 => [-5, -4, -3, -2, -1, 0, 1, 2, 3, 4, 5, 6, 7, 9, 11, 13],
        24000 => [-5, -3, -2, -1, 0, 1, 2, 3, 4, 5, 6, 7, 9, 11, 13, 16],
        32000 => [-6, -4, -2, -1, 0, 1, 2, 3, 4, 5, 6, 7, 9, 11, 13, 16],
        44100 => [-4, -2, -1, 0, 1, 2, 3, 4, 5, 6, 7, 9, 11, 13, 16, 20],
        48000 => [-4, -2, -1, 0, 1, 2, 3, 4, 5, 6, 7, 9, 11, 13, 16, 20],
        64000 => [-4, -2, -1, 0, 1, 2, 3, 4, 5, 6, 7, 9, 11, 13, 16, 20],
        88200 => [-2, -1, 0, 1, 2, 3, 4, 5, 6, 7, 9, 11, 13, 16, 20, 24],
        96000 => [-2, -1, 0, 1, 2, 3, 4, 5, 6, 7, 9, 11, 13, 16, 20, 24],
        _ => return None,
    })
}

/// The QMF band a 3, 4 or 5 kHz border falls in: `(start_min, stop_min)`.
fn start_stop_min(rate: u32) -> (i64, i64) {
    let temp: i64 = if rate < 32000 {
        3000
    } else if rate < 64000 {
        4000
    } else {
        5000
    };
    let rate = rate as i64;
    (
        ((temp << 7) + (rate >> 1)) / rate,
        ((temp << 8) + (rate >> 1)) / rate,
    )
}

/// The widest `k2 - k0` reconstructed at this rate.
fn span_limit(rate: u32) -> i64 {
    if rate < 32000 {
        48
    } else if rate < 64000 {
        35
    } else {
        32
    }
}

/// `num` band widths geometrically spaced from `start` to `stop`.
fn make_bands(start: i64, stop: i64, num: i64) -> Vec<i64> {
    let base = (stop as f64 / start as f64).powf(1.0 / num as f64);
    let mut prod = start as f64;
    let mut previous = start;
    let mut out = Vec::new();
    for _ in 0..num - 1 {
        prod *= base;
        let present = (prod + 0.5).floor() as i64;
        out.push(present - previous);
        previous = present;
    }
    out.push(stop - previous);
    out
}

/// The master frequency band table.
fn f_master(
    rate: u32,
    start_freq: usize,
    stop_freq: usize,
    freq_scale: i32,
    alter_scale: i32,
) -> Option<Vec<i64>> {
    let offsets = k0_offsets(rate)?;
    let (start_min, stop_min) = start_stop_min(rate);
    let k0 = start_min + offsets[start_freq] as i64;
    let k2 = if stop_freq < 14 {
        let mut widths = make_bands(stop_min, 64, 13);
        widths.sort();
        stop_min + widths[..stop_freq].iter().sum::<i64>()
    } else if stop_freq == 14 {
        2 * k0
    } else {
        3 * k0
    };
    let k2 = k2.min(64);
    if k2 <= k0 || k2 - k0 > span_limit(rate) {
        return None;
    }

    if freq_scale == 0 {
        let dk: i64 = if alter_scale != 0 { 2 } else { 1 };
        let num_bands = 2 * ((k2 - k0 + 2 * (dk - 1)) / (2 * dk));
        if num_bands <= 0 {
            return None;
        }
        let mut vk = vec![dk; num_bands as usize];
        let mut k2diff = k2 - k0 - num_bands * dk;
        let (mut at, incr): (i64, i64) = if k2diff < 0 {
            (0, 1)
        } else {
            (num_bands - 1, -1)
        };
        while k2diff != 0 && at >= 0 && at < num_bands {
            vk[at as usize] -= incr;
            at += incr;
            k2diff += incr;
        }
        let mut table = vec![k0];
        for d in vk {
            if d <= 0 {
                return None;
            }
            table.push(table[table.len() - 1] + d);
        }
        return Some(table);
    }

    let half_bands = 7 - freq_scale;
    let two_regions = 49 * k2 > 110 * k0;
    let k1 = if two_regions { 2 * k0 } else { k2 };
    let num_bands_0 = 2 * (half_bands as f64 * (k1 as f64 / k0 as f64).log2() + 0.5).floor() as i64;
    if num_bands_0 <= 0 {
        return None;
    }
    let mut vk0 = make_bands(k0, k1, num_bands_0);
    vk0.sort();
    let vdk0_max = vk0[vk0.len() - 1];
    let mut table = vec![k0];
    for &d in &vk0 {
        if d <= 0 {
            return None;
        }
        table.push(table[table.len() - 1] + d);
    }
    if !two_regions {
        return Some(table);
    }
    let invwarp = if alter_scale != 0 { 1.0 / 1.3 } else { 1.0 };
    let num_bands_1 =
        2 * (half_bands as f64 * invwarp * (k2 as f64 / k1 as f64).log2() + 0.5).floor() as i64;
    if num_bands_1 <= 0 {
        return None;
    }
    let mut vk1 = make_bands(k1, k2, num_bands_1);
    vk1.sort();
    if vdk0_max > vk1[0] {
        let change = (vdk0_max - vk1[0]).min((vk1[vk1.len() - 1] - vk1[0]) >> 1);
        let last = vk1.len() - 1;
        vk1[0] += change;
        vk1[last] -= change;
        vk1.sort();
    }
    for &d in &vk1 {
        if d <= 0 {
            return None;
        }
        table.push(table[table.len() - 1] + d);
    }
    Some(table)
}

/// The derived band tables for one SBR header, or `None` if the reference
/// decoder would have refused this combination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BandTables {
    pub n_master: usize,
    pub n_high: usize,
    pub n_low: usize,
    pub n_q: usize,
    pub f_high: Vec<i64>,
    pub f_low: Vec<i64>,
    pub f_noise: Vec<i64>,
    pub kx: i64,
    pub k2: i64,
    /// Master table borders (`k0 = f_master[0]`), needed by the patch builder.
    pub f_master: Vec<i64>,
    /// SBR (output) sample rate, the patch builder's `goalSb` input.
    pub rate: u32,
}

/// `rate` is the SBR rate: twice the AAC-LC core's.
pub fn freq_tables(
    rate: u32,
    start_freq: usize,
    stop_freq: usize,
    freq_scale: i32,
    alter_scale: i32,
    xover_band: usize,
    noise_bands: i32,
) -> Option<BandTables> {
    let master = f_master(rate, start_freq, stop_freq, freq_scale, alter_scale)?;
    let n_master = master.len() - 1;
    if xover_band > n_master {
        return None;
    }
    let n_high = n_master - xover_band;
    if n_high < 1 {
        return None;
    }
    let n_low = (n_high + 1) >> 1;
    let f_high = master[xover_band..].to_vec();
    let mut f_low = vec![f_high[0]];
    if n_high & 1 != 0 {
        for k in 1..=n_low {
            f_low.push(f_high[2 * k - 1]);
        }
    } else {
        for k in 1..=n_low {
            f_low.push(f_high[2 * k]);
        }
    }
    let kx = f_high[0];
    let k2 = master[master.len() - 1];
    let n_q = (1i64.max((noise_bands as f64 * (k2 as f64 / kx as f64).log2() + 0.5).floor() as i64))
        as usize;
    if n_q > 5 {
        return None;
    }
    let mut f_noise = vec![f_low[0]];
    let mut temp = 0i64;
    for k in 1..=n_q {
        temp += (n_low as i64 - temp) / (n_q as i64 + 1 - k as i64);
        f_noise.push(f_low[temp as usize]);
    }
    Some(BandTables {
        n_master,
        n_high,
        n_low,
        n_q,
        f_high,
        f_low,
        f_noise,
        kx,
        k2,
        f_master: master.clone(),
        rate,
    })
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod vectors;
