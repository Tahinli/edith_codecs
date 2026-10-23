//! Legacy audio decoders: IMA ADPCM (WAV `0x0011` and QuickTime `ima4`),
//! Microsoft ADPCM (`0x0002`) and G.711 A-law / mu-law, all delivering `s16`.
//!
//! Every block-based format here decodes a packet of whole blocks: the
//! container hands one block (WAV/AVI `nBlockAlign` bytes, or a run of 34-byte
//! QuickTime packets) per packet, and a ragged tail is dropped the way the
//! oracle's decoders drop it. Output is interleaved `i16`, bit-exact against
//! ffmpeg's `adpcm_ima_wav`, `adpcm_ima_qt`, `adpcm_ms`, `pcm_alaw` and
//! `pcm_mulaw`.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use ec_core::error::{Error, Result};
use ec_core::frame::{AudioFrame, ChannelLayout, Frame, SampleFormat};
use ec_core::packet::{Buf, Packet};
use ec_core::registry::{AudioParameters, CodecId, CodecParameters, Decoder, MediaParameters};

const STEP_TABLE: [i32; 89] = [
    7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66,
    73, 80, 88, 97, 107, 118, 130, 143, 157, 173, 190, 209, 230, 253, 279, 307, 337, 371, 408,
    449, 494, 544, 598, 658, 724, 796, 876, 963, 1060, 1166, 1282, 1411, 1552, 1707, 1878, 2066,
    2272, 2499, 2749, 3024, 3327, 3660, 4026, 4428, 4871, 5358, 5894, 6484, 7132, 7845, 8630,
    9493, 10442, 11487, 12635, 13899, 15289, 16818, 18500, 20350, 22385, 24623, 27086, 29794,
    32767,
];
const INDEX_TABLE: [i32; 8] = [-1, -1, -1, -1, 2, 4, 6, 8];

const MS_COEF1: [i32; 7] = [256, 512, 0, 192, 240, 460, 392];
const MS_COEF2: [i32; 7] = [0, -256, 0, 64, 0, -208, -232];
const MS_ADAPT: [i32; 16] = [
    230, 230, 230, 230, 307, 409, 512, 614, 768, 614, 512, 409, 307, 230, 230, 230,
];

/// Which format a decoder reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// IMA ADPCM in WAV/AVI (`0x0011`).
    ImaWav,
    /// Microsoft ADPCM (`0x0002`).
    MsAdpcm,
    /// QuickTime IMA4.
    ImaQt,
    /// G.711 A-law.
    Alaw,
    /// G.711 mu-law.
    Mulaw,
}

impl Kind {
    /// The decoder kind for a codec id, if this crate decodes it.
    pub fn of(codec: CodecId) -> Option<Kind> {
        Some(match codec {
            CodecId::ImaAdpcmWav => Kind::ImaWav,
            CodecId::MsAdpcm => Kind::MsAdpcm,
            CodecId::ImaAdpcmQt => Kind::ImaQt,
            CodecId::PcmAlaw => Kind::Alaw,
            CodecId::PcmMulaw => Kind::Mulaw,
            _ => return None,
        })
    }
}

fn clip16(v: i32) -> i32 {
    v.clamp(-32768, 32767)
}

#[derive(Clone, Copy, Debug, Default)]
struct Ima {
    predictor: i32,
    index: i32,
}

impl Ima {
    /// One nibble, bitwise step accumulation (ffmpeg's QT and 4-bit WAV path).
    fn expand(&mut self, nibble: u8) -> i16 {
        let step = STEP_TABLE[self.index as usize];
        let mut diff = step >> 3;
        if nibble & 4 != 0 {
            diff += step;
        }
        if nibble & 2 != 0 {
            diff += step >> 1;
        }
        if nibble & 1 != 0 {
            diff += step >> 2;
        }
        self.predictor = clip16(if nibble & 8 != 0 {
            self.predictor - diff
        } else {
            self.predictor + diff
        });
        self.index = (self.index + INDEX_TABLE[usize::from(nibble & 7)]).clamp(0, 88);
        self.predictor as i16
    }
}

#[derive(Clone, Copy, Default)]
struct Ms {
    c1: i32,
    c2: i32,
    delta: i32,
    s1: i32,
    s2: i32,
}

impl Ms {
    fn expand(&mut self, nibble: u8) -> i16 {
        let mut p = self
            .s1
            .wrapping_mul(self.c1)
            .wrapping_add(self.s2.wrapping_mul(self.c2))
            / 256;
        let n = i32::from(nibble);
        p = p.wrapping_add((if n & 8 != 0 { n - 16 } else { n }).wrapping_mul(self.delta));
        self.s2 = self.s1;
        self.s1 = clip16(p);
        self.delta = (MS_ADAPT[usize::from(nibble)].wrapping_mul(self.delta) >> 8)
            .clamp(16, i32::MAX / 768);
        self.s1 as i16
    }
}

fn alaw(v: u8) -> i16 {
    let a = v ^ 0x55;
    let mut t = i32::from(a & 0x7f);
    if t < 16 {
        t = (t << 4) + 8;
    } else {
        let seg = t >> 4;
        t = ((t & 0x0f) << 4) + 0x108;
        t <<= seg - 1;
    }
    (if a & 0x80 != 0 { t } else { -t }) as i16
}

fn mulaw(v: u8) -> i16 {
    let u = !v;
    let mut t = (i32::from(u & 0x0f) << 3) + 0x84;
    t <<= (u & 0x70) >> 4;
    (if u & 0x80 != 0 { 0x84 - t } else { t - 0x84 }) as i16
}

/// A decoder for one stream of any [`Kind`].
#[derive(Debug)]
pub struct PcmExtDecoder {
    kind: Kind,
    channels: usize,
    sample_rate: u32,
    coefs: Vec<(i32, i32)>,
    params: CodecParameters,
    out: Vec<i16>,
    /// IMA4 running state per channel, kept across packets the way the
    /// oracle keeps it (a header that agrees with it does not reset it).
    qt: Vec<Ima>,
    pending: Option<AudioFrame>,
    drained: bool,
}

impl PcmExtDecoder {
    /// A decoder for the stream `params` describes. For MS ADPCM the
    /// extradata is the `fmt ` tail (samples per block, coefficient count and
    /// pairs); without one the standard seven pairs are used.
    pub fn from_parameters(params: CodecParameters) -> Result<PcmExtDecoder> {
        let kind = Kind::of(params.codec)
            .ok_or_else(|| Error::corrupt("ec-pcm-ext: not one of its codecs"))?;
        let audio = params
            .audio()
            .ok_or_else(|| Error::corrupt("ec-pcm-ext: a non-audio stream"))?;
        let channels = audio.layout.channel_count().max(1);
        let sample_rate = audio.sample_rate;
        let mut coefs: Vec<(i32, i32)> = MS_COEF1.iter().copied().zip(MS_COEF2).collect();
        if kind == Kind::MsAdpcm
            && let Some(x) = params.extradata.as_deref()
            && x.len() >= 4
        {
            let n = usize::from(u16::from_le_bytes([x[2], x[3]]));
            if n > 0 && x.len() >= 4 + n * 4 {
                coefs = (0..n)
                    .map(|i| {
                        let b = &x[4 + i * 4..];
                        (
                            i32::from(i16::from_le_bytes([b[0], b[1]])),
                            i32::from(i16::from_le_bytes([b[2], b[3]])),
                        )
                    })
                    .collect();
            }
        }
        if kind == Kind::MsAdpcm && channels > 2 {
            return Err(Error::unsupported(
                "MS ADPCM with more than two channels",
                "the format is mono or stereo",
            ));
        }
        let mut p = params;
        p.media = MediaParameters::Audio(AudioParameters {
            sample_rate,
            layout: ChannelLayout::from_count(channels),
            format: Some(SampleFormat::S16),
            bits_per_sample: Some(16),
        });
        Ok(PcmExtDecoder {
            kind,
            channels,
            sample_rate,
            coefs,
            params: p,
            out: Vec::new(),
            qt: vec![Ima::default(); channels],
            pending: None,
            drained: false,
        })
    }

    /// Decode one packet into interleaved `i16`. The slice is valid until the
    /// next call.
    pub fn decode(&mut self, data: &[u8]) -> Result<&[i16]> {
        self.out.clear();
        let ch = self.channels;
        match self.kind {
            Kind::Alaw => self.out.extend(data.iter().map(|&b| alaw(b))),
            Kind::Mulaw => self.out.extend(data.iter().map(|&b| mulaw(b))),
            Kind::ImaWav => {
                if data.len() < 4 * ch {
                    return Err(Error::corrupt("IMA ADPCM: a block shorter than its header"));
                }
                let groups = (data.len() - 4 * ch) / (4 * ch);
                let n = 1 + groups * 8;
                let mut st = vec![Ima::default(); ch];
                self.out.resize(n * ch, 0);
                for (c, s) in st.iter_mut().enumerate() {
                    let h = &data[c * 4..];
                    s.predictor = i32::from(i16::from_le_bytes([h[0], h[1]]));
                    s.index = i32::from(i16::from_le_bytes([h[2], h[3]]));
                    if !(0..=88).contains(&s.index) {
                        self.out.clear();
                        return Err(Error::corrupt("IMA ADPCM: step index out of range"));
                    }
                    self.out[c] = s.predictor as i16;
                }
                let body = &data[4 * ch..];
                for g in 0..groups {
                    for (c, s) in st.iter_mut().enumerate() {
                        let src = &body[(g * ch + c) * 4..][..4];
                        for (m, &b) in src.iter().enumerate() {
                            let at = 1 + g * 8 + m * 2;
                            self.out[at * ch + c] = s.expand(b & 15);
                            self.out[(at + 1) * ch + c] = s.expand(b >> 4);
                        }
                    }
                }
            }
            Kind::ImaQt => {
                let blocks = data.len() / (34 * ch);
                self.out.resize(blocks * 64 * ch, 0);
                for b in 0..blocks {
                    for c in 0..ch {
                        let src = &data[(b * ch + c) * 34..][..34];
                        let h = u16::from_be_bytes([src[0], src[1]]);
                        let mut s = Ima {
                            predictor: i32::from((h & 0xFF80) as i16),
                            index: i32::from(h & 0x7F),
                        };
                        if s.index > 88 {
                            self.out.clear();
                            return Err(Error::corrupt("IMA4: step index out of range"));
                        }
                        // The header carries only the top 9 bits of the
                        // predictor: when the step index agrees with the
                        // running state and the predictor is the truncation of
                        // it, the running (full precision) value stands.
                        let run = self.qt[c];
                        if run.index == s.index && (0..=0x7f).contains(&(run.predictor - s.predictor)) {
                            s.predictor = run.predictor;
                        }
                        for (m, &v) in src[2..].iter().enumerate() {
                            let at = b * 64 + m * 2;
                            self.out[at * ch + c] = s.expand(v & 15);
                            self.out[(at + 1) * ch + c] = s.expand(v >> 4);
                        }
                        self.qt[c] = s;
                    }
                }
            }
            Kind::MsAdpcm => {
                if ch > 2 {
                    return Err(Error::unsupported("MS ADPCM", "more than two channels"));
                }
                if data.len() < 7 * ch {
                    return Err(Error::corrupt("MS ADPCM: a block shorter than its header"));
                }
                let mut st = [Ms::default(); 2];
                let mut at = 0;
                for s in st.iter_mut().take(ch) {
                    let bp = usize::from(data[at]);
                    at += 1;
                    let &(c1, c2) = self
                        .coefs
                        .get(bp)
                        .ok_or_else(|| Error::corrupt("MS ADPCM: block predictor out of range"))?;
                    s.c1 = c1;
                    s.c2 = c2;
                }
                let rd = |at: &mut usize| {
                    let v = i32::from(i16::from_le_bytes([data[*at], data[*at + 1]]));
                    *at += 2;
                    v
                };
                for s in st.iter_mut().take(ch) {
                    s.delta = rd(&mut at);
                }
                for s in st.iter_mut().take(ch) {
                    s.s1 = rd(&mut at);
                }
                for s in st.iter_mut().take(ch) {
                    s.s2 = rd(&mut at);
                }
                for s in st.iter().take(ch) {
                    self.out.push(s.s2 as i16);
                }
                for s in st.iter().take(ch) {
                    self.out.push(s.s1 as i16);
                }
                let stereo = ch == 2;
                for &b in &data[at..] {
                    self.out.push(st[0].expand(b >> 4));
                    self.out.push(st[usize::from(stereo)].expand(b & 15));
                }
            }
        }
        Ok(&self.out)
    }

    /// Sample rate the stream declared.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
}

impl Decoder for PcmExtDecoder {
    fn codec_parameters(&self) -> &CodecParameters {
        &self.params
    }

    fn send_packet(&mut self, packet: &Packet) -> Result<()> {
        self.decode(&packet.data)?;
        let channels = self.channels;
        let mut bytes = Vec::with_capacity(self.out.len() * 2);
        for &s in &self.out {
            bytes.extend_from_slice(&s.to_ne_bytes());
        }
        let mut frame = AudioFrame::try_new(
            SampleFormat::S16,
            false,
            ChannelLayout::from_count(channels),
            self.sample_rate,
            self.out.len() / channels,
            vec![Buf::from_vec(bytes)],
        )?;
        frame.pts = packet
            .pts
            .map(|ticks| ec_core::timebase::Timestamp::new(ticks, packet.time_base));
        self.pending = Some(frame);
        Ok(())
    }

    fn receive_frame(&mut self) -> Result<Frame> {
        match self.pending.take() {
            Some(frame) => Ok(Frame::Audio(frame)),
            None if self.drained => Err(Error::Eof),
            None => Err(Error::NeedMore),
        }
    }

    fn flush(&mut self) -> Result<()> {
        self.drained = true;
        Ok(())
    }

    fn reset(&mut self) {
        self.qt.iter_mut().for_each(|s| *s = Ima::default());
        self.pending = None;
        self.drained = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn g711_anchor_values() {
        assert_eq!(alaw(0xD5), 8);
        assert_eq!(alaw(0x55), -8);
        assert_eq!(mulaw(0xFF), 0);
        assert_eq!(mulaw(0x00), -32124);
        assert_eq!(mulaw(0x80), 32124);
    }
}
