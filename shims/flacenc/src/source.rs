//! `flacenc::source`: where samples come from.

/// A signal the encoder can read.
///
/// The incumbent's trait is a pull interface with a reusable context; the
/// replica only ever hands over a [`MemSource`] it built from a `Vec`, so this
/// is the whole-buffer shape of the same thing.
pub trait Source {
    /// Channel count.
    fn channels(&self) -> usize;
    /// Bits per sample.
    fn bits_per_sample(&self) -> usize;
    /// Sample rate in Hz.
    fn sample_rate(&self) -> usize;
    /// The interleaved samples.
    fn as_raw_slice(&self) -> &[i32];
}

/// Samples already in memory — `flacenc::source::MemSource`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemSource {
    channels: usize,
    bits_per_sample: usize,
    sample_rate: usize,
    samples: Vec<i32>,
}

impl MemSource {
    /// Build from interleaved samples.
    pub fn from_samples(
        samples: &[i32],
        channels: usize,
        bits_per_sample: usize,
        sample_rate: usize,
    ) -> Self {
        MemSource {
            channels,
            bits_per_sample,
            sample_rate,
            samples: samples.to_owned(),
        }
    }

    /// The samples, interleaved.
    pub fn as_raw_slice(&self) -> &[i32] {
        &self.samples
    }

    /// Inter-channel samples held.
    pub fn len(&self) -> usize {
        match self.channels {
            0 => 0,
            n => self.samples.len() / n,
        }
    }

    /// True when the source holds no samples.
    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }
}

impl Source for MemSource {
    fn channels(&self) -> usize {
        self.channels
    }

    fn bits_per_sample(&self) -> usize {
        self.bits_per_sample
    }

    fn sample_rate(&self) -> usize {
        self.sample_rate
    }

    fn as_raw_slice(&self) -> &[i32] {
        &self.samples
    }
}
