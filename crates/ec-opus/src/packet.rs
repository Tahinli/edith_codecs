//! Opus packet framing: the TOC byte, codes 0-3, and the self-delimiting
//! variant (RFC 6716, Section 3 and Appendix B).
//!
//! Every rule marked `[R1]`..`[R7]` in Section 3.4 is enforced here and reported as
//! [`Error::Corrupt`] — a malformed packet is rejected at the framing layer, so
//! nothing downstream ever sees a frame it cannot account for.
//!
//! The self-delimiting variant is what makes multichannel work: a multistream
//! packet is several ordinary Opus packets concatenated, all but the last
//! carrying their own length. [`Packet::parse`] with `self_delimited = true`
//! reports how many bytes it consumed so the next stream can start there.

use ec_core::{Error, Result};

/// Which of the three operating modes a configuration selects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// SILK only: NB/MB/WB speech, 10-60 ms.
    Silk,
    /// SILK below 8 kHz plus CELT above it: SWB/FB, 10 or 20 ms.
    Hybrid,
    /// CELT only: NB..FB, 2.5-20 ms.
    Celt,
}

/// Audio bandwidth, in the RFC's names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Bandwidth {
    /// Narrowband, 4 kHz.
    Narrow,
    /// Mediumband, 6 kHz.
    Medium,
    /// Wideband, 8 kHz.
    Wide,
    /// Super-wideband, 12 kHz.
    SuperWide,
    /// Fullband, 20 kHz.
    Full,
}

impl Bandwidth {
    /// The CELT band count that covers this bandwidth (Section 4.3).
    pub fn celt_end_band(self) -> usize {
        match self {
            Bandwidth::Narrow => 13,
            Bandwidth::Medium | Bandwidth::Wide => 17,
            Bandwidth::SuperWide => 19,
            Bandwidth::Full => 21,
        }
    }
}

/// The parsed TOC byte (Section 3.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Toc {
    /// Configuration number, 0..32.
    pub config: u8,
    /// True when the packet carries two channels.
    pub stereo: bool,
    /// Frame count code, 0..4.
    pub code: u8,
}

impl Toc {
    /// Splits a TOC byte into its fields.
    pub fn new(byte: u8) -> Toc {
        Toc {
            config: byte >> 3,
            stereo: byte & 0x4 != 0,
            code: byte & 0x3,
        }
    }

    /// Operating mode for this configuration.
    pub fn mode(self) -> Mode {
        match self.config {
            0..=11 => Mode::Silk,
            12..=15 => Mode::Hybrid,
            _ => Mode::Celt,
        }
    }

    /// Audio bandwidth for this configuration.
    pub fn bandwidth(self) -> Bandwidth {
        match self.config {
            0..=3 => Bandwidth::Narrow,
            4..=7 => Bandwidth::Medium,
            8..=11 => Bandwidth::Wide,
            12..=13 => Bandwidth::SuperWide,
            14..=15 => Bandwidth::Full,
            16..=19 => Bandwidth::Narrow,
            20..=23 => Bandwidth::Wide,
            24..=27 => Bandwidth::SuperWide,
            _ => Bandwidth::Full,
        }
    }

    /// Samples per frame at 48 kHz.
    pub fn frame_size_48k(self) -> usize {
        match self.config {
            // SILK: 10, 20, 40, 60 ms.
            0..=11 => [480, 960, 1920, 2880][(self.config & 0x3) as usize],
            // Hybrid: 10, 20 ms.
            12..=15 => [480, 960][(self.config & 0x1) as usize],
            // CELT: 2.5, 5, 10, 20 ms.
            _ => [120, 240, 480, 960][(self.config & 0x3) as usize],
        }
    }

    /// Channel count signalled by the stereo flag.
    pub fn channels(self) -> usize {
        if self.stereo { 2 } else { 1 }
    }
}

/// One parsed Opus packet: its TOC and the byte range of each Opus frame.
#[derive(Clone, Debug)]
pub struct Packet<'a> {
    /// The table-of-contents byte.
    pub toc: Toc,
    /// Compressed frames, in order. A zero-length frame is DTX.
    pub frames: Vec<&'a [u8]>,
    /// Bytes of the input this packet occupied — the offset of the next
    /// stream when parsing self-delimited packets.
    pub consumed: usize,
}

/// Reads a one- or two-byte frame length (Section 3.2.1).
fn frame_length(data: &[u8], pos: &mut usize) -> Result<usize> {
    let b0 = *data
        .get(*pos)
        .ok_or_else(|| Error::corrupt("opus packet: truncated frame length"))?
        as usize;
    *pos += 1;
    if b0 < 252 {
        return Ok(b0);
    }
    let b1 = *data
        .get(*pos)
        .ok_or_else(|| Error::corrupt("opus packet: truncated two-byte frame length"))?
        as usize;
    *pos += 1;
    Ok(b1 * 4 + b0)
}

impl<'a> Packet<'a> {
    /// Parses one packet. `self_delimited` selects the Appendix B variant, in
    /// which one extra length precedes the first frame's data.
    ///
    /// Rejects every malformed packet listed in Section 3.4 rather than
    /// guessing at a repair.
    pub fn parse(data: &'a [u8], self_delimited: bool) -> Result<Packet<'a>> {
        // [R1] Packets are at least one byte.
        if data.is_empty() {
            return Err(Error::corrupt("opus packet: empty"));
        }
        let toc = Toc::new(data[0]);
        let mut pos = 1usize;
        let mut frames = Vec::new();
        // Total duration cap of 120 ms [R5].
        let max_frames = 5760 / toc.frame_size_48k();

        match toc.code {
            0 => {
                let len = if self_delimited {
                    frame_length(data, &mut pos)?
                } else {
                    data.len() - pos
                };
                push_frame(data, &mut pos, len, &mut frames)?;
            }
            1 => {
                let len = if self_delimited {
                    frame_length(data, &mut pos)?
                } else {
                    // [R3] the payload must split evenly in two.
                    let rest = data.len() - pos;
                    if !rest.is_multiple_of(2) {
                        return Err(Error::corrupt("opus packet: code 1 with odd payload"));
                    }
                    rest / 2
                };
                push_frame(data, &mut pos, len, &mut frames)?;
                push_frame(data, &mut pos, len, &mut frames)?;
            }
            2 => {
                // [R4] first length must fit in what remains.
                let n1 = frame_length(data, &mut pos)?;
                let n2 = if self_delimited {
                    frame_length(data, &mut pos)?
                } else {
                    (data.len() - pos)
                        .checked_sub(n1)
                        .ok_or_else(|| Error::corrupt("opus packet: code 2 first frame overruns"))?
                };
                push_frame(data, &mut pos, n1, &mut frames)?;
                push_frame(data, &mut pos, n2, &mut frames)?;
            }
            _ => {
                // [R6,R7] code 3 needs the frame count byte.
                let fc = *data
                    .get(pos)
                    .ok_or_else(|| Error::corrupt("opus packet: code 3 without frame count"))?;
                pos += 1;
                let vbr = fc & 0x80 != 0;
                let padded = fc & 0x40 != 0;
                let count = (fc & 0x3F) as usize;
                // [R5] at least one frame, at most 120 ms of audio.
                if count == 0 || count > max_frames {
                    return Err(Error::corrupt(format!(
                        "opus packet: code 3 frame count {count} (max {max_frames})"
                    )));
                }
                let mut padding = 0usize;
                if padded {
                    loop {
                        let p = *data.get(pos).ok_or_else(|| {
                            Error::corrupt("opus packet: truncated padding length")
                        })?;
                        pos += 1;
                        padding += p as usize;
                        if p != 255 {
                            break;
                        }
                        padding -= 1;
                    }
                }
                // Padding bytes live at the very end of the packet. Where the
                // packet end is only known from the frame lengths (Appendix B),
                // they are skipped after the frames instead.
                let body = if self_delimited {
                    data
                } else {
                    let end = data
                        .len()
                        .checked_sub(padding)
                        .filter(|end| *end >= pos)
                        .ok_or_else(|| Error::corrupt("opus packet: padding larger than packet"))?;
                    &data[..end]
                };
                if vbr {
                    let mut lengths = Vec::with_capacity(count);
                    for _ in 0..count - 1 {
                        lengths.push(frame_length(body, &mut pos)?);
                    }
                    let last = if self_delimited {
                        frame_length(body, &mut pos)?
                    } else {
                        let used: usize = lengths.iter().sum();
                        (body.len() - pos).checked_sub(used).ok_or_else(|| {
                            Error::corrupt("opus packet: code 3 VBR lengths overrun")
                        })?
                    };
                    lengths.push(last);
                    for len in lengths {
                        push_frame(body, &mut pos, len, &mut frames)?;
                    }
                } else {
                    let len = if self_delimited {
                        frame_length(body, &mut pos)?
                    } else {
                        // [R6] (N-2-P) must be a multiple of M.
                        let rest = body.len() - pos;
                        if !rest.is_multiple_of(count) {
                            return Err(Error::corrupt(
                                "opus packet: code 3 CBR payload not a multiple of the frame count",
                            ));
                        }
                        rest / count
                    };
                    for _ in 0..count {
                        push_frame(body, &mut pos, len, &mut frames)?;
                    }
                }
                pos += padding;
            }
        }

        // [R2] no frame exceeds 1275 bytes.
        if let Some(f) = frames.iter().find(|f| f.len() > 1275) {
            return Err(Error::corrupt(format!(
                "opus packet: frame of {} bytes exceeds 1275",
                f.len()
            )));
        }
        if !self_delimited && pos != data.len() {
            return Err(Error::corrupt(format!(
                "opus packet: {} trailing bytes",
                data.len() - pos
            )));
        }
        Ok(Packet {
            toc,
            frames,
            consumed: pos,
        })
    }

    /// Samples this packet decodes to at 48 kHz.
    pub fn samples_48k(&self) -> usize {
        self.frames.len() * self.toc.frame_size_48k()
    }
}

fn push_frame<'a>(
    data: &'a [u8],
    pos: &mut usize,
    len: usize,
    frames: &mut Vec<&'a [u8]>,
) -> Result<()> {
    let end = pos
        .checked_add(len)
        .filter(|end| *end <= data.len())
        .ok_or_else(|| Error::corrupt("opus packet: frame runs past the end of the packet"))?;
    frames.push(&data[*pos..end]);
    *pos = end;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toc_table_2() {
        // Spot-checks of Table 2, one per row plus the frame-size ordering.
        assert_eq!(Toc::new(0).mode(), Mode::Silk);
        assert_eq!(Toc::new(0).frame_size_48k(), 480);
        assert_eq!(Toc::new(3 << 3).frame_size_48k(), 2880);
        assert_eq!(Toc::new(4 << 3).bandwidth(), Bandwidth::Medium);
        assert_eq!(Toc::new(12 << 3).mode(), Mode::Hybrid);
        assert_eq!(Toc::new(12 << 3).bandwidth(), Bandwidth::SuperWide);
        assert_eq!(Toc::new(13 << 3).frame_size_48k(), 960);
        assert_eq!(Toc::new(15 << 3).bandwidth(), Bandwidth::Full);
        assert_eq!(Toc::new(16 << 3).mode(), Mode::Celt);
        assert_eq!(Toc::new(16 << 3).frame_size_48k(), 120);
        assert_eq!(Toc::new(31 << 3).bandwidth(), Bandwidth::Full);
        assert!(Toc::new((31 << 3) | 4).stereo);
        assert_eq!(Toc::new((31 << 3) | 4 | 3).code, 3);
    }

    #[test]
    fn code0_to_code3() {
        // Code 0: everything after the TOC is one frame.
        let p = Packet::parse(&[0, 0xaa, 0xbb], false).unwrap();
        assert_eq!(p.frames, vec![&[0xaa, 0xbb][..]]);

        // Code 1: two halves.
        let p = Packet::parse(&[1, 1, 2, 3, 4], false).unwrap();
        assert_eq!(p.frames, vec![&[1, 2][..], &[3, 4][..]]);
        assert!(Packet::parse(&[1, 1, 2, 3], false).is_err(), "[R3]");

        // Code 2: explicit first length.
        let p = Packet::parse(&[2, 1, 0x11, 0x22, 0x33], false).unwrap();
        assert_eq!(p.frames, vec![&[0x11][..], &[0x22, 0x33][..]]);
        assert!(Packet::parse(&[2, 9, 0x11], false).is_err(), "[R4]");

        // Code 3 CBR, 3 frames of 2 bytes.
        let p = Packet::parse(&[3, 3, 1, 2, 3, 4, 5, 6], false).unwrap();
        assert_eq!(p.frames.len(), 3);
        assert_eq!(p.frames[2], &[5, 6][..]);
        assert!(
            Packet::parse(&[3, 3, 1, 2, 3, 4, 5], false).is_err(),
            "[R6]"
        );
        assert!(Packet::parse(&[3, 0], false).is_err(), "[R5] zero frames");

        // Code 3 VBR with padding: lengths 1 and 2, then 2 padding bytes.
        let p = Packet::parse(
            &[3, 0x80 | 0x40 | 3, 2, 1, 2, 0xa, 0xb, 0xc, 0xd, 0, 0],
            false,
        )
        .unwrap();
        assert_eq!(p.frames, vec![&[0xa][..], &[0xb, 0xc][..], &[0xd][..]]);

        // [R1]
        assert!(Packet::parse(&[], false).is_err());
    }

    #[test]
    fn self_delimited_leaves_the_rest_for_the_next_stream() {
        // Two self-delimited code 0 packets back to back, then an undelimited one.
        let data = [0, 2, 0xa, 0xb, 0, 1, 0xc, 0, 0xd, 0xe];
        let first = Packet::parse(&data, true).unwrap();
        assert_eq!(first.frames, vec![&[0xa, 0xb][..]]);
        assert_eq!(first.consumed, 4);
        let second = Packet::parse(&data[first.consumed..], true).unwrap();
        assert_eq!(second.frames, vec![&[0xc][..]]);
        assert_eq!(second.consumed, 3);
        let last = Packet::parse(&data[first.consumed + second.consumed..], false).unwrap();
        assert_eq!(last.frames, vec![&[0xd, 0xe][..]]);

        // Code 1 self-delimited: the length applies to both frames.
        let p = Packet::parse(&[1, 2, 1, 2, 3, 4, 0xff], true).unwrap();
        assert_eq!(p.frames, vec![&[1, 2][..], &[3, 4][..]]);
        assert_eq!(p.consumed, 6);

        // Code 2 self-delimited: both lengths explicit.
        let p = Packet::parse(&[2, 1, 2, 0x11, 0x22, 0x33, 0xff], true).unwrap();
        assert_eq!(p.frames, vec![&[0x11][..], &[0x22, 0x33][..]]);
        assert_eq!(p.consumed, 6);
    }

    #[test]
    fn malformed_input_is_an_error_not_a_panic() {
        // Every prefix of a well-formed packet, and a fuzz of short buffers.
        let good = [3u8, 0x80 | 0x40 | 3, 2, 1, 2, 0xa, 0xb, 0xc, 0xd, 0, 0];
        for n in 0..good.len() {
            let _ = Packet::parse(&good[..n], false);
            let _ = Packet::parse(&good[..n], true);
        }
        let mut x = 1u32;
        for _ in 0..5000 {
            let mut buf = Vec::new();
            for _ in 0..(x % 17) {
                x = x.wrapping_mul(1664525).wrapping_add(1013904223);
                buf.push((x >> 16) as u8);
            }
            let _ = Packet::parse(&buf, false);
            let _ = Packet::parse(&buf, true);
        }
    }
}
