//! Residue decode (§8): the spectrum that rides on the floor.
//!
//! All three residue types share one loop — partitions classified in groups,
//! then up to eight refinement passes over those classes — and differ only in
//! how a partition's values are laid out. Type 2 differs one level up: it codes
//! every channel of a submap as one interleaved vector, which is what makes
//! coupled stereo cheap.

use crate::bits::Bits;
use crate::codebook::Codebook;
use crate::setup::ResidueConfig;

/// Decode one residue into `vectors`, which the caller has zeroed.
///
/// For types 0 and 1 there is one vector per channel of the submap; for type 2
/// the caller passes a single interleaved vector `channels * n` long. Channels
/// flagged in `skip` are not coded at all (an unused floor, or a coupling
/// partner already covered).
pub fn decode(
    config: &ResidueConfig,
    codebooks: &[Codebook],
    bits: &mut Bits,
    vectors: &mut [Vec<f32>],
    skip: &[bool],
) {
    let Some(size) = vectors.first().map(Vec::len) else {
        return;
    };
    if skip.iter().all(|&s| s) {
        return;
    }
    let begin = config.begin.min(size);
    let end = config.end.min(size);
    let to_read = end.saturating_sub(begin);
    let partitions = to_read / config.partition_size;
    if partitions == 0 {
        return;
    }
    let classbook = &codebooks[config.classbook];
    let per_word = classbook.dimensions.max(1);
    let classes = config.classifications as u32;
    let mut classification = vec![vec![0u8; partitions + per_word]; vectors.len()];

    for pass in 0..8usize {
        let mut partition = 0usize;
        while partition < partitions {
            if pass == 0 {
                for (channel, row) in classification.iter_mut().enumerate() {
                    if skip[channel] {
                        continue;
                    }
                    let Some(mut word) = classbook.decode_scalar(bits) else {
                        return;
                    };
                    // The class word packs `per_word` classes, most recent
                    // partition in the least significant digit.
                    for i in (0..per_word).rev() {
                        row[partition + i] = (word % classes) as u8;
                        word /= classes;
                    }
                }
            }
            for _ in 0..per_word {
                if partition >= partitions {
                    break;
                }
                for (channel, vector) in vectors.iter_mut().enumerate() {
                    if skip[channel] {
                        continue;
                    }
                    let class = usize::from(classification[channel][partition]);
                    let book = config.books[class][pass];
                    if book < 0 {
                        continue;
                    }
                    let offset = begin + partition * config.partition_size;
                    let slice = &mut vector[offset..offset + config.partition_size];
                    if !decode_partition(config.kind, &codebooks[book as usize], bits, slice) {
                        return;
                    }
                }
                partition += 1;
            }
        }
    }
}

/// One partition's worth of values, added onto what earlier passes left.
fn decode_partition(kind: u8, book: &Codebook, bits: &mut Bits, out: &mut [f32]) -> bool {
    if !book.has_values() {
        return false;
    }
    let dimensions = book.dimensions;
    match kind {
        // Type 0 lays a codeword's values out strided, so one codeword covers
        // the whole partition at `step` spacing.
        0 => {
            let step = out.len() / dimensions.max(1);
            for i in 0..step {
                let Some(values) = book.decode_vector(bits) else {
                    return false;
                };
                for (j, &value) in values.iter().enumerate() {
                    if let Some(slot) = out.get_mut(i + j * step) {
                        *slot += value;
                    }
                }
            }
        }
        // Types 1 and 2 lay them out contiguously.
        _ => {
            let mut i = 0usize;
            while i < out.len() {
                let Some(values) = book.decode_vector(bits) else {
                    return false;
                };
                for &value in values {
                    if i >= out.len() {
                        break;
                    }
                    out[i] += value;
                    i += 1;
                }
                if dimensions == 0 {
                    return false;
                }
            }
        }
    }
    true
}

/// Spread a type-2 interleaved vector back over its channels.
pub fn deinterleave(interleaved: &[f32], channels: usize, out: &mut [Vec<f32>]) {
    for (i, &value) in interleaved.iter().enumerate() {
        let channel = i % channels;
        let position = i / channels;
        if let Some(slot) = out[channel].get_mut(position) {
            *slot = value;
        }
    }
}
