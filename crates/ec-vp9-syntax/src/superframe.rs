//! The superframe index (spec Annex B).
//!
//! A VP9 "chunk" — one sample in an MP4 or Matroska file, one IVF frame — may
//! hold several coded frames, at most one of which is shown. That is how a
//! hidden ALTREF travels: it is packed ahead of the frame that references it,
//! and the frame that eventually displays it is a bodiless
//! `show_existing_frame` header. The packing is described by an index appended
//! *after* the frames, whose first and last bytes are the same marker byte, so
//! it can be found by reading the chunk backwards.

use ec_core::{Error, Result};

/// Split a chunk into its coded frames.
///
/// A chunk without a superframe index is one frame, returned as a single slice
/// borrowing the whole input — so callers never need to branch on "is this a
/// superframe". Sizes that do not add up to the chunk length are
/// [`Error::Corrupt`]: a decoder that trusted them would read another frame's
/// bytes.
pub fn split(chunk: &[u8]) -> Result<Vec<&[u8]>> {
    let Some(&marker) = chunk.last() else {
        return Err(Error::corrupt("VP9 superframe: empty chunk"));
    };
    if marker & 0xe0 != 0xc0 {
        return Ok(vec![chunk]);
    }
    let bytes_per_size = ((marker >> 3) & 0x3) as usize + 1;
    let frames = (marker & 0x7) as usize + 1;
    let index_size = 2 + bytes_per_size * frames;
    if chunk.len() < index_size {
        return Ok(vec![chunk]);
    }
    let index = &chunk[chunk.len() - index_size..];
    // The marker is repeated at both ends of the index; a byte that merely looks
    // like a marker by accident almost never is.
    if index[0] != marker {
        return Ok(vec![chunk]);
    }

    let mut out = Vec::with_capacity(frames);
    let mut offset = 0usize;
    for i in 0..frames {
        let mut size = 0usize;
        for b in 0..bytes_per_size {
            size |= (index[1 + i * bytes_per_size + b] as usize) << (8 * b);
        }
        let end = offset
            .checked_add(size)
            .filter(|&end| end <= chunk.len() - index_size)
            .ok_or_else(|| {
                Error::corrupt(format!(
                    "VP9 superframe: frame {i} of {frames} runs past the chunk ({size} bytes at {offset})"
                ))
            })?;
        out.push(&chunk[offset..end]);
        offset = end;
    }
    Ok(out)
}

/// True if `chunk` carries a superframe index, i.e. holds more than one frame.
pub fn is_superframe(chunk: &[u8]) -> bool {
    matches!(split(chunk), Ok(frames) if frames.len() > 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a chunk of `sizes` dummy frames plus a well-formed index.
    fn pack(sizes: &[usize], bytes_per_size: usize) -> Vec<u8> {
        let mut out = Vec::new();
        for (i, &s) in sizes.iter().enumerate() {
            out.extend(std::iter::repeat_n(i as u8, s));
        }
        let marker = 0xc0 | ((bytes_per_size as u8 - 1) << 3) | (sizes.len() as u8 - 1);
        out.push(marker);
        for &s in sizes {
            for b in 0..bytes_per_size {
                out.push((s >> (8 * b)) as u8);
            }
        }
        out.push(marker);
        out
    }

    #[test]
    fn plain_chunk_is_one_frame() {
        let frames = split(&[0x82, 0x49, 0x83, 0x42]).unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].len(), 4);
    }

    #[test]
    fn splits_at_the_coded_sizes() {
        let chunk = pack(&[3, 5, 300], 2);
        let frames = split(&chunk).unwrap();
        assert_eq!(
            frames.iter().map(|f| f.len()).collect::<Vec<_>>(),
            [3, 5, 300]
        );
        assert_eq!(frames[2][0], 2);
    }

    #[test]
    fn oversized_entry_is_corrupt() {
        let mut chunk = pack(&[3, 5], 1);
        let last_size = chunk.len() - 2;
        chunk[last_size] = 0xff; // second frame claims 255 bytes
        assert!(matches!(split(&chunk), Err(Error::Corrupt { .. })));
    }

    #[test]
    fn marker_without_matching_head_is_not_an_index() {
        let mut chunk = pack(&[3, 5], 1);
        let head = chunk.len() - (2 + 2); // 2 size bytes + 2 marker copies
        chunk[head] = 0x00; // clobber the leading marker copy
        assert_eq!(split(&chunk).unwrap().len(), 1);
    }

    #[test]
    fn empty_chunk_is_corrupt() {
        assert!(split(&[]).is_err());
    }
}
