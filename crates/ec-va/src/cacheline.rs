//! Which cache lines a CPU write-back of a mapped DMA-BUF has to visit.
//!
//! `CLFLUSH` takes an address and writes back the whole cache line holding it,
//! so a buffer needs one flush per line, not one per byte. The incumbent
//! stack flushed every byte (64 instructions per line, 6.2 ms per 1080p
//! picture on radeonsi). The base of a mapping is not guaranteed to be cache
//! aligned, so the last byte's line is flushed too: a range that starts
//! mid-line can straddle one more line than its length suggests.

/// Bytes per cache line on every x86-64 part this crate targets.
pub const CACHE_LINE: usize = 64;

/// Offsets from a mapping's base at which to issue a flush for a buffer of
/// `len` bytes: one per [`CACHE_LINE`], plus the last byte when the stride
/// does not land on it. Empty for `len == 0`, which has no line to flush.
pub fn flush_offsets(len: usize) -> Vec<usize> {
    if len == 0 {
        return Vec::new();
    }
    let last = len - 1;
    let mut offsets: Vec<usize> = (0..len).step_by(CACHE_LINE).collect();
    if offsets.last() != Some(&last) {
        offsets.push(last);
    }
    offsets
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The absolute cache-line indices a flush at `offsets` writes back, for a
    /// mapping whose base sits `base_mod` bytes into a line.
    fn lines_flushed(base_mod: usize, offsets: &[usize]) -> Vec<usize> {
        let base = 1000 * CACHE_LINE + base_mod;
        let mut lines: Vec<usize> = offsets.iter().map(|o| (base + o) / CACHE_LINE).collect();
        lines.sort_unstable();
        lines.dedup();
        lines
    }

    #[test]
    fn empty_buffer_has_nothing_to_flush() {
        assert!(flush_offsets(0).is_empty());
    }

    #[test]
    fn one_flush_per_line_not_per_byte() {
        assert_eq!(flush_offsets(1), vec![0]);
        // 64 bytes from an unaligned base can still straddle two lines: the last byte too.
        assert_eq!(flush_offsets(64), vec![0, 63]);
        assert_eq!(flush_offsets(65), vec![0, 64]);
        assert_eq!(flush_offsets(128), vec![0, 64, 127]);
        // A 1080p NV12 picture (~3.1 MB) is ~48K flushes, not ~3.1M.
        assert!(flush_offsets(1920 * 1620).len() <= 1920 * 1620 / CACHE_LINE + 2);
    }

    // Regression for the straddle: with an unaligned base, the bytes of a
    // range can sit in one more line than a stride from the base reaches, so
    // every byte's line must be among the flushed lines for every alignment.
    #[test]
    fn every_byte_is_covered_for_every_base_alignment() {
        for base_mod in 0..CACHE_LINE {
            for len in 0..=300 {
                let offsets = flush_offsets(len);
                let flushed = lines_flushed(base_mod, &offsets);
                let base = 1000 * CACHE_LINE + base_mod;
                for i in 0..len {
                    let line = (base + i) / CACHE_LINE;
                    assert!(
                        flushed.contains(&line),
                        "base_mod {base_mod} len {len}: byte {i} (line {line}) not flushed"
                    );
                }
            }
        }
    }
}
