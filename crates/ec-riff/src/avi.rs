//! RIFF/AVI audio chunk reader.
//!
//! This is the small half an audio probe needs: stream headers plus `movi`
//! sound chunks. Picture chunks are skipped.

use std::collections::VecDeque;
use std::io::{Read, Seek, SeekFrom};

use ec_core::{Error, Result};

/// One audio stream declared by an AVI file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AviAudioStream {
    /// AVI stream number, the two decimal digits at the start of chunk ids.
    pub index: u32,
    /// WAVE format tag from this stream's `strf` body.
    pub format_tag: u16,
    /// Channel count stated by the WAVEFORMAT header.
    pub channels: u16,
    /// Samples per second stated by the WAVEFORMAT header.
    pub sample_rate: u32,
    /// Block alignment stated by the WAVEFORMAT header.
    pub block_align: u16,
    /// Bits per sample stated by the WAVEFORMAT header.
    pub bits_per_sample: u16,
}

/// One audio chunk from `movi`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AviPacket {
    /// AVI stream number.
    pub stream: u32,
    /// Chunk payload.
    pub data: Vec<u8>,
}

/// A reader over AVI audio chunks.
pub struct AviReader<R> {
    inner: R,
    streams: Vec<AviAudioStream>,
    movi_end: u64,
    pos: u64,
    pending: VecDeque<AviPacket>,
}

impl<R: Read + Seek> AviReader<R> {
    /// Parse the RIFF/AVI headers, leaving the reader at the start of `movi`.
    pub fn new(mut inner: R) -> Result<AviReader<R>> {
        let end = inner.seek(SeekFrom::End(0))?;
        inner.rewind()?;
        let mut head = [0u8; 12];
        read_full(&mut inner, &mut head)?;
        if &head[0..4] != b"RIFF" || &head[8..12] != b"AVI " {
            return Err(Error::corrupt("not a RIFF/AVI file"));
        }

        let mut streams = Vec::new();
        let mut movi = None;
        let mut pos = 12u64;
        while pos + 8 <= end {
            inner.seek(SeekFrom::Start(pos))?;
            let mut chunk = [0u8; 8];
            let got = read_some(&mut inner, &mut chunk)?;
            if got == 0 {
                break;
            }
            if got < 8 {
                return Err(Error::corrupt("AVI chunk header ends mid-field"));
            }
            let id = [chunk[0], chunk[1], chunk[2], chunk[3]];
            let size = u32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]) as u64;
            let data = pos + 8;
            let next = data.saturating_add(size).saturating_add(size & 1);
            if &id == b"LIST" {
                let mut kind = [0u8; 4];
                if size < 4 || read_some(&mut inner, &mut kind)? < 4 {
                    return Err(Error::corrupt("AVI LIST header ends mid-field"));
                }
                let body_start = data + 4;
                let body_end = data.saturating_add(size).min(end);
                match &kind {
                    b"hdrl" => streams = parse_hdrl(&mut inner, body_start, body_end)?,
                    b"movi" => movi = Some((body_start, body_end)),
                    _ => {}
                }
            }
            pos = next;
        }
        let (pos, movi_end) = movi.ok_or_else(|| Error::corrupt("AVI: no movi list"))?;
        Ok(AviReader {
            inner,
            streams,
            movi_end,
            pos,
            pending: VecDeque::new(),
        })
    }

    /// Audio streams declared in the header.
    pub fn audio_streams(&self) -> &[AviAudioStream] {
        &self.streams
    }

    /// The next audio chunk, or [`Error::Eof`] at the end.
    pub fn next_packet(&mut self) -> Result<AviPacket> {
        if let Some(packet) = self.pending.pop_front() {
            return Ok(packet);
        }
        while self.pos + 8 <= self.movi_end {
            self.inner.seek(SeekFrom::Start(self.pos))?;
            let mut chunk = [0u8; 8];
            let got = read_some(&mut self.inner, &mut chunk)?;
            if got == 0 || got < 8 {
                self.pos = self.movi_end;
                return Err(Error::Eof);
            }
            let id = [chunk[0], chunk[1], chunk[2], chunk[3]];
            let size = u32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]) as u64;
            let data = self.pos + 8;
            let padded = size.saturating_add(size & 1);
            let next = data.saturating_add(padded);
            if next > self.movi_end {
                self.pos = self.movi_end;
                return Err(Error::Eof);
            }
            self.pos = next;
            if &id == b"LIST" {
                if size >= 4 {
                    let mut kind = [0u8; 4];
                    if read_some(&mut self.inner, &mut kind)? < 4 {
                        self.pos = self.movi_end;
                        return Err(Error::Eof);
                    }
                    if &kind == b"rec " {
                        self.queue_packets_in(data + 4, data + size)?;
                        if let Some(packet) = self.pending.pop_front() {
                            return Ok(packet);
                        }
                    }
                }
                continue;
            }
            let Some(stream) = stream_number(&id).filter(|_| &id[2..4] == b"wb") else {
                continue;
            };
            if !self.streams.iter().any(|s| s.index == stream) {
                continue;
            }
            let data_buf = self.read_packet_body(data, size)?;
            return Ok(AviPacket {
                stream,
                data: data_buf,
            });
        }
        Err(Error::Eof)
    }

    fn queue_packets_in(&mut self, start: u64, end: u64) -> Result<()> {
        let mut pos = start;
        while pos + 8 <= end {
            self.inner.seek(SeekFrom::Start(pos))?;
            let mut chunk = [0u8; 8];
            if read_some(&mut self.inner, &mut chunk)? < 8 {
                break;
            }
            let id = [chunk[0], chunk[1], chunk[2], chunk[3]];
            let size = u32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]) as u64;
            let data = pos + 8;
            let next = data.saturating_add(size).saturating_add(size & 1);
            if next > end {
                break;
            }
            if &id == b"LIST" {
                if size >= 4 {
                    let mut kind = [0u8; 4];
                    if read_some(&mut self.inner, &mut kind)? < 4 {
                        break;
                    }
                    if &kind == b"rec " {
                        self.queue_packets_in(data + 4, data + size)?;
                    }
                }
            } else if let Some(stream) = stream_number(&id).filter(|_| &id[2..4] == b"wb")
                && self.streams.iter().any(|s| s.index == stream)
            {
                let data_buf = self.read_packet_body(data, size)?;
                self.pending.push_back(AviPacket {
                    stream,
                    data: data_buf,
                });
            }
            pos = next;
        }
        Ok(())
    }

    fn read_packet_body(&mut self, data: u64, size: u64) -> Result<Vec<u8>> {
        let mut data_buf = vec![0u8; size as usize];
        self.inner.seek(SeekFrom::Start(data))?;
        if read_some(&mut self.inner, &mut data_buf)? < data_buf.len() {
            self.pos = self.movi_end;
            return Err(Error::Eof);
        }
        Ok(data_buf)
    }
}

fn parse_hdrl<R: Read + Seek>(inner: &mut R, start: u64, end: u64) -> Result<Vec<AviAudioStream>> {
    let mut streams = Vec::new();
    let mut stream_no = 0u32;
    let mut pos = start;
    while pos + 8 <= end {
        inner.seek(SeekFrom::Start(pos))?;
        let mut chunk = [0u8; 8];
        if read_some(inner, &mut chunk)? < 8 {
            break;
        }
        let id = [chunk[0], chunk[1], chunk[2], chunk[3]];
        let size = u32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]) as u64;
        let data = pos + 8;
        let next = data.saturating_add(size).saturating_add(size & 1);
        if &id == b"LIST" {
            let mut kind = [0u8; 4];
            if size >= 4 && read_some(inner, &mut kind)? == 4 && &kind == b"strl" {
                if let Some(stream) = parse_strl(inner, stream_no, data + 4, data + size)? {
                    streams.push(stream);
                }
                stream_no += 1;
            }
        }
        pos = next;
    }
    Ok(streams)
}

fn parse_strl<R: Read + Seek>(
    inner: &mut R,
    stream_no: u32,
    start: u64,
    end: u64,
) -> Result<Option<AviAudioStream>> {
    let mut audio = false;
    let mut fmt = None;
    let mut pos = start;
    while pos + 8 <= end {
        inner.seek(SeekFrom::Start(pos))?;
        let mut chunk = [0u8; 8];
        if read_some(inner, &mut chunk)? < 8 {
            break;
        }
        let id = [chunk[0], chunk[1], chunk[2], chunk[3]];
        let size = u32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]) as u64;
        let data = pos + 8;
        let next = data.saturating_add(size).saturating_add(size & 1);
        match &id {
            b"strh" => {
                let mut body = vec![0u8; size.min(64) as usize];
                read_full(inner, &mut body)?;
                audio = body.get(0..4) == Some(&b"auds"[..]);
            }
            b"strf" => {
                let mut body = vec![0u8; size.min(18) as usize];
                read_full(inner, &mut body)?;
                if body.len() >= 16 {
                    fmt = Some(AviAudioStream {
                        index: stream_no,
                        format_tag: u16::from_le_bytes([body[0], body[1]]),
                        channels: u16::from_le_bytes([body[2], body[3]]),
                        sample_rate: u32::from_le_bytes([body[4], body[5], body[6], body[7]]),
                        block_align: u16::from_le_bytes([body[12], body[13]]),
                        bits_per_sample: u16::from_le_bytes([body[14], body[15]]),
                    });
                }
            }
            _ => {}
        }
        pos = next;
    }
    Ok(audio.then_some(fmt).flatten())
}

fn stream_number(id: &[u8; 4]) -> Option<u32> {
    let a = id[0].checked_sub(b'0')?;
    let b = id[1].checked_sub(b'0')?;
    (a < 10 && b < 10).then_some(u32::from(a) * 10 + u32::from(b))
}

fn read_some(r: &mut impl Read, buf: &mut [u8]) -> Result<usize> {
    let mut at = 0;
    while at < buf.len() {
        match r.read(&mut buf[at..]) {
            Ok(0) => break,
            Ok(n) => at += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(Error::Io(e)),
        }
    }
    Ok(at)
}

fn read_full(r: &mut impl Read, buf: &mut [u8]) -> Result<()> {
    if read_some(r, buf)? == buf.len() {
        Ok(())
    } else {
        Err(Error::corrupt("AVI header ends mid-field"))
    }
}
