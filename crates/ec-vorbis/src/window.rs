//! Vorbis windows (§4.3.1), including the asymmetric long-block shapes.
//!
//! A long block next to a short one does not use the long slope: it uses the
//! *short* slope, centred where the short block's own slope will be, and is
//! flat or zero either side of it. That is what makes the two windows still add
//! to one across the overlap, and it is the whole reason a Vorbis decoder needs
//! four long-window variants rather than one.

/// The Vorbis slope of `n` samples: `sin(pi/2 * sin^2((i + 1/2)/n * pi/2))`,
/// rising from 0 to 1.
fn slope(n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| {
            let x = (i as f64 + 0.5) / n as f64 * std::f64::consts::FRAC_PI_2;
            let s = x.sin();
            (std::f64::consts::FRAC_PI_2 * s * s).sin() as f32
        })
        .collect()
}

/// One window of `n` samples for a block whose neighbours are long or short.
///
/// `short` is the stream's short blocksize; it is what sets the slope width
/// when a neighbour is short. For a short block itself both flags are true by
/// construction (its slopes are already the narrow ones).
pub fn build(n: usize, short: usize, previous_long: bool, next_long: bool) -> Vec<f32> {
    let mut window = vec![0.0f32; n];
    let (left_start, left_n) = match previous_long {
        true => (0usize, n / 2),
        false => (n / 4 - short / 4, short / 2),
    };
    let (right_start, right_n) = match next_long {
        true => (n / 2, n / 2),
        false => (n * 3 / 4 - short / 4, short / 2),
    };
    let left = slope(left_n);
    let right = slope(right_n);
    for (i, value) in left.iter().enumerate() {
        window[left_start + i] = *value;
    }
    window[left_start + left_n..right_start].fill(1.0);
    // The right slope is the left one read backwards, which is what makes the
    // pair Princen-Bradley.
    for i in 0..right_n {
        window[right_start + i] = right[right_n - 1 - i];
    }
    window
}

/// The four long-block variants plus the short one, built once per stream.
pub struct Windows {
    short: Vec<f32>,
    long: [Vec<f32>; 4],
}

impl Windows {
    /// Build every window a stream with these blocksizes can ask for.
    pub fn new(blocksize_0: usize, blocksize_1: usize) -> Windows {
        Windows {
            short: build(blocksize_0, blocksize_0, true, true),
            long: [
                build(blocksize_1, blocksize_0, false, false),
                build(blocksize_1, blocksize_0, false, true),
                build(blocksize_1, blocksize_0, true, false),
                build(blocksize_1, blocksize_0, true, true),
            ],
        }
    }

    /// The window for one block.
    pub fn get(&self, long: bool, previous_long: bool, next_long: bool) -> &[f32] {
        match long {
            false => &self.short,
            true => &self.long[usize::from(previous_long) * 2 + usize::from(next_long)],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlapping_windows_add_to_one() {
        // Long block against long block: the two slopes are the classic
        // Princen-Bradley pair, so squares sum to one across the overlap.
        let long = build(2048, 256, true, true);
        for i in 0..1024 {
            let sum = long[1024 + i] * long[1024 + i] + long[i] * long[i];
            assert!((sum - 1.0).abs() < 1e-6, "{i}: {sum}");
        }
        // Long block against a short neighbour: the long window's right slope
        // is the short window's own slope, sitting where the short block will
        // put its left one, and is zero past it.
        let to_short = build(2048, 256, true, false);
        let short = build(256, 256, true, true);
        assert!(to_short[2047] == 0.0 && to_short[1600] == 0.0);
        for i in 0..128 {
            let sum = to_short[1472 + i] * to_short[1472 + i] + short[i] * short[i];
            assert!((sum - 1.0).abs() < 1e-6, "{i}: {sum}");
        }
    }
}
