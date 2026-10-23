//! The raw `.amr` / `.awb` storage format (RFC 4867 section 5): a magic line,
//! then one frame after another, each a table-of-contents byte followed by the
//! speech bits.
//!
//! Packets keep the ToC byte in front of the payload, which is also the form the
//! decoders take, so a packet is exactly one stored frame.

use std::io::{Read, Seek, SeekFrom};

use ec_core::error::{Error, Result};
use ec_core::frame::ChannelLayout;
use ec_core::packet::{Buf, Packet, PacketFlags};
use ec_core::registry::{
    AudioParameters, CodecId, CodecParameters, Demuxer, MediaParameters, SeekMode, StreamInfo,
};
use ec_core::timebase::{Rounding, TimeBase, Timestamp};

/// Magic of a narrowband file.
pub const MAGIC_NB: &[u8] = b"#!AMR\n";
/// Magic of a wideband file.
pub const MAGIC_WB: &[u8] = b"#!AMR-WB\n";

/// Narrowband or wideband.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    /// AMR-NB: 8 kHz, 160 samples per frame.
    Nb,
    /// AMR-WB: 16 kHz, 320 samples per frame.
    Wb,
}

impl Kind {
    /// Sample rate in Hz.
    pub fn sample_rate(self) -> u32 {
        match self {
            Kind::Nb => 8000,
            Kind::Wb => 16000,
        }
    }

    /// Samples in one 20 ms frame.
    pub fn frame_samples(self) -> i64 {
        match self {
            Kind::Nb => 160,
            Kind::Wb => 320,
        }
    }

    /// The codec carried.
    pub fn codec(self) -> CodecId {
        match self {
            Kind::Nb => CodecId::AmrNb,
            Kind::Wb => CodecId::AmrWb,
        }
    }

    /// The file magic.
    pub fn magic(self) -> &'static [u8] {
        match self {
            Kind::Nb => MAGIC_NB,
            Kind::Wb => MAGIC_WB,
        }
    }
}

/// The kind of file `buf` opens as, by its magic.
pub fn probe(buf: &[u8]) -> Option<Kind> {
    if buf.starts_with(MAGIC_WB) {
        Some(Kind::Wb)
    } else if buf.starts_with(MAGIC_NB) {
        Some(Kind::Nb)
    } else {
        None
    }
}

/// Payload bytes (after the ToC byte) of an AMR-NB frame of type `ft`.
pub fn nb_frame_size(ft: u8) -> Result<usize> {
    const SIZES: [usize; 9] = [12, 13, 15, 17, 19, 20, 26, 31, 5];
    match ft {
        0..=8 => Ok(SIZES[usize::from(ft)]),
        15 => Ok(0),
        _ => Err(Error::corrupt(format!("AMR-NB frame type {ft} is reserved"))),
    }
}

/// Payload bytes (after the ToC byte) of an AMR-WB frame of type `ft`.
pub fn wb_frame_size(ft: u8) -> Result<usize> {
    const SIZES: [usize; 10] = [17, 23, 32, 36, 40, 46, 50, 58, 60, 5];
    match ft {
        0..=9 => Ok(SIZES[usize::from(ft)]),
        14 | 15 => Ok(0),
        _ => Err(Error::corrupt(format!("AMR-WB frame type {ft} is reserved"))),
    }
}

/// Payload bytes of a frame of type `ft` in `kind`.
pub fn frame_size(kind: Kind, ft: u8) -> Result<usize> {
    match kind {
        Kind::Nb => nb_frame_size(ft),
        Kind::Wb => wb_frame_size(ft),
    }
}

/// Reads as many bytes as available; returns the count.
fn read_upto(r: &mut impl Read, buf: &mut [u8]) -> Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(n)
}

/// A demuxer for `.amr` / `.awb` files.
pub struct AmrDemuxer<R> {
    r: R,
    kind: Kind,
    streams: Vec<StreamInfo>,
    /// Offset of the first frame.
    start: u64,
    /// Index of the next frame.
    next: u64,
    /// Offset of the next frame.
    pos: u64,
    /// Frame offsets, built on the first seek.
    index: Option<Vec<u64>>,
}

impl<R: Read + Seek + Send> AmrDemuxer<R> {
    /// Open a file positioned at its magic.
    pub fn new(mut r: R) -> Result<AmrDemuxer<R>> {
        let mut head = [0u8; MAGIC_WB.len()];
        let n = read_upto(&mut r, &mut head)?;
        let kind = probe(&head[..n]).ok_or_else(|| Error::corrupt("not an AMR storage file"))?;
        // Fails with Unsupported while a kind's frame table is missing.
        frame_size(kind, 0)?;
        let start = kind.magic().len() as u64;
        r.seek(SeekFrom::Start(start))?;
        let mut audio = AudioParameters {
            sample_rate: kind.sample_rate(),
            layout: ChannelLayout::Mono,
            ..AudioParameters::default()
        };
        audio.bits_per_sample = None;
        let mut params = CodecParameters::new(kind.codec());
        params.media = MediaParameters::Audio(audio);
        let streams = vec![StreamInfo::new(0, TimeBase::from_rate(kind.sample_rate()), params)];
        Ok(AmrDemuxer { r, kind, streams, start, next: 0, pos: start, index: None })
    }

    /// Which flavour was opened.
    pub fn kind(&self) -> Kind {
        self.kind
    }

    /// Read the ToC at the current position and its frame; `Ok(None)` at a clean or truncated end.
    fn read_frame(&mut self) -> Result<Option<Vec<u8>>> {
        let mut toc = [0u8; 1];
        if read_upto(&mut self.r, &mut toc)? == 0 {
            return Ok(None);
        }
        let ft = (toc[0] >> 3) & 0x0F;
        let size = match frame_size(self.kind, ft) {
            Ok(s) => s,
            Err(e) => {
                self.pos += 1;
                return Err(e);
            }
        };
        let mut data = vec![0u8; 1 + size];
        data[0] = toc[0];
        if read_upto(&mut self.r, &mut data[1..])? < size {
            return Ok(None);
        }
        self.pos += 1 + size as u64;
        Ok(Some(data))
    }

    fn build_index(&mut self) -> Result<()> {
        if self.index.is_some() {
            return Ok(());
        }
        let mut offsets = Vec::new();
        let mut pos = self.start;
        let mut toc = [0u8; 1];
        let len = self.r.seek(SeekFrom::End(0))?;
        loop {
            self.r.seek(SeekFrom::Start(pos))?;
            if read_upto(&mut self.r, &mut toc)? == 0 {
                break;
            }
            let Ok(size) = frame_size(self.kind, (toc[0] >> 3) & 0x0F) else {
                pos += 1;
                continue;
            };
            let end = pos + 1 + size as u64;
            if end > len {
                break;
            }
            offsets.push(pos);
            pos = end;
        }
        self.index = Some(offsets);
        Ok(())
    }
}

impl<R: Read + Seek + Send> Demuxer for AmrDemuxer<R> {
    fn streams(&self) -> &[StreamInfo] {
        &self.streams
    }

    fn next_packet(&mut self) -> Result<Packet> {
        let Some(data) = self.read_frame()? else {
            return Err(Error::Eof);
        };
        let samples = self.kind.frame_samples();
        let mut p = Packet::new(0, TimeBase::from_rate(self.kind.sample_rate()), Buf::from_vec(data))
            .with_pts(self.next as i64 * samples)
            .with_duration(samples);
        p.dts = p.pts;
        p.flags = PacketFlags { keyframe: true, ..PacketFlags::default() };
        self.next += 1;
        Ok(p)
    }

    fn seek(&mut self, stream: u32, to: Timestamp, mode: SeekMode) -> Result<()> {
        if stream != 0 {
            return Err(Error::corrupt(format!("no stream {stream}")));
        }
        self.build_index()?;
        let base = TimeBase::from_rate(self.kind.sample_rate());
        let samples = self.kind.frame_samples();
        let rounding = match mode {
            SeekMode::SyncAfter => Rounding::Up,
            _ => Rounding::Down,
        };
        let ticks = to.base.rescale(to.ticks, base, rounding).max(0);
        let mut frame = match mode {
            SeekMode::SyncAfter => (ticks + samples - 1) / samples,
            _ => ticks / samples,
        } as usize;
        let index = self.index.as_ref().map_or(&[][..], |v| v.as_slice());
        frame = frame.min(index.len());
        let offset = index.get(frame).copied();
        match offset {
            Some(o) => self.pos = o,
            None => {
                // Past the last frame: land at the end of the last one.
                self.pos = index.last().map_or(self.start, |_| u64::MAX);
            }
        }
        if self.pos == u64::MAX {
            self.r.seek(SeekFrom::End(0))?;
        } else {
            self.r.seek(SeekFrom::Start(self.pos))?;
        }
        self.next = frame as u64;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn file(frames: &[u8]) -> Vec<u8> {
        let mut v = MAGIC_NB.to_vec();
        v.extend_from_slice(frames);
        v
    }

    #[test]
    fn reads_frames_and_stops_at_truncation() {
        // mode 0 (12 bytes), SID (5 bytes), no-data, then a truncated mode 7.
        let mut f = vec![0x04];
        f.extend([1u8; 12]);
        f.push(0x44);
        f.extend([2u8; 5]);
        f.push(0x7C);
        f.extend([0x3C, 9, 9]);
        let mut d = AmrDemuxer::new(Cursor::new(file(&f))).unwrap();
        assert_eq!(d.next_packet().unwrap().data.len(), 13);
        let p = d.next_packet().unwrap();
        assert_eq!((p.data.len(), p.pts), (6, Some(160)));
        assert_eq!(d.next_packet().unwrap().data.len(), 1);
        assert!(d.next_packet().unwrap_err().is_eof());
    }

    #[test]
    fn reserved_type_errors_and_wb_probes() {
        let mut d = AmrDemuxer::new(Cursor::new(file(&[0x4C, 0x04]))).unwrap();
        assert!(matches!(d.next_packet(), Err(Error::Corrupt { .. })));
        assert!(AmrDemuxer::new(Cursor::new(b"#!AMR-WB\n".to_vec())).is_ok());
        assert_eq!(wb_frame_size(8).unwrap(), 60);
        assert_eq!(wb_frame_size(9).unwrap(), 5);
        assert!(wb_frame_size(11).is_err());
        assert_eq!(probe(b"#!AMR-WB\nxx"), Some(Kind::Wb));
    }

    #[test]
    fn seek_lands_on_frame() {
        let mut f = Vec::new();
        for i in 0..5u8 {
            f.push(0x04);
            f.extend([i; 12]);
        }
        let mut d = AmrDemuxer::new(Cursor::new(file(&f))).unwrap();
        d.seek(0, Timestamp::new(350, TimeBase::from_rate(8000)), SeekMode::SyncBefore).unwrap();
        let p = d.next_packet().unwrap();
        assert_eq!((p.pts, p.data[1]), (Some(320), 2));
    }
}
