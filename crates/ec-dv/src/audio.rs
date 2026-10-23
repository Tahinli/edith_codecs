//! DV audio: the PCM carried in the audio DIF blocks (port of `dv_extract_audio` in FFmpeg's
//! `libavformat/dv.c`). 16-bit linear and 12-bit non-linear data both come out as 16-bit linear.

use crate::profile::{Profile, frame_profile};

const DV_AUDIO_SOURCE: u8 = 0x50;
const FREQUENCIES: [u32; 3] = [48000, 44100, 32000];

/// The audio of one DV frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DvAudio {
    /// Sample rate in Hz (48000, 44100 or 32000).
    pub sample_rate: u32,
    /// Samples per channel in this frame.
    pub samples: usize,
    /// True when the source was 12-bit non-linear (already expanded to 16-bit linear).
    pub nonlinear_12bit: bool,
    /// One entry per stereo pair announced by the audio source pack (1, 2 or 4). Each is
    /// interleaved L/R `i16`, `2 * samples` long; empty when the pair is not carried by this
    /// frame (the second half of a 720p frame pair).
    pub pairs: Vec<Vec<i16>>,
}

fn audio_12to16(sample: u16) -> u16 {
    let sample = if sample < 0x800 { sample } else { sample | 0xf000 };
    let shift = (sample & 0xf00) >> 8;
    if !(0x2..=0xd).contains(&shift) {
        sample
    } else if shift < 0x8 {
        let shift = shift - 1;
        sample.wrapping_sub(256 * shift).wrapping_shl(u32::from(shift))
    } else {
        let shift = 0xe - shift;
        (sample.wrapping_add(256 * shift + 1).wrapping_shl(u32::from(shift))).wrapping_sub(1)
    }
}

fn extract_pack(frame: &[u8], t: u8) -> Option<usize> {
    for c in 0..10usize {
        let offs = if c & 1 == 1 { 80 * 6 + 3 + c * 12000 } else { 80 * 6 + 80 * 16 * 3 + 3 + c * 12000 };
        if frame.get(offs) == Some(&t) {
            return Some(offs);
        }
    }
    None
}

/// Extract the audio of a DV frame. `None` when the frame carries no (decodable) audio.
pub fn extract_audio(frame: &[u8]) -> Option<DvAudio> {
    let sys = frame_profile(None, frame)?;
    if frame.len() < sys.frame_size {
        return None;
    }
    extract_audio_with(frame, sys)
}

pub(crate) fn extract_audio_with(frame: &[u8], sys: &Profile) -> Option<DvAudio> {
    let off = extract_pack(frame, DV_AUDIO_SOURCE)?;
    let smpls = usize::from(frame[off + 1] & 0x3f);
    let freq = usize::from(frame[off + 4] >> 3 & 7);
    let stype = frame[off + 3] & 0x1f;
    let quant = frame[off + 4] & 7;
    if freq >= FREQUENCIES.len() || stype > 3 || quant > 1 {
        return None;
    }
    let sr = FREQUENCIES[freq];
    let mut ach = [1usize, 0, 2, 4][usize::from(stype)];
    if ach == 1 && quant != 0 && freq == 2 {
        ach = 2;
    }
    if ach == 0 {
        return None;
    }
    let size = (sys.audio_min_samples[freq] + smpls) * 4;
    let half_ch = sys.difseg_size / 2;
    let mut ipcm = if sys.height == 720 && frame[1] & 0x0c == 0 { 2 } else { 0 };
    let mut pcm: Vec<Vec<u8>> = vec![vec![0u8; 8192]; ach.max(4)];
    let available = ach;

    if ipcm + sys.n_difchan <= if quant == 1 { 2 } else { 4 } {
        let mut pos = 0usize;
        'chans: for _chan in 0..sys.n_difchan {
            let mut cur = ipcm;
            ipcm += 1;
            if cur >= available {
                break;
            }
            for i in 0..sys.difseg_size {
                pos += 6 * 80;
                if quant == 1 && i == half_ch {
                    cur = ipcm;
                    ipcm += 1;
                    if cur >= available {
                        break 'chans;
                    }
                }
                for j in 0..9 {
                    let mut d = 8;
                    while d < 80 {
                        let f = |k: usize| frame[pos + k];
                        if quant == 0 {
                            let of = usize::from(sys.audio_shuffle[i][j]) + (d - 8) / 2 * sys.audio_stride;
                            if of * 2 < size {
                                pcm[cur][of * 2] = f(d + 1);
                                pcm[cur][of * 2 + 1] = f(d);
                                if pcm[cur][of * 2 + 1] == 0x80 && pcm[cur][of * 2] == 0x00 {
                                    pcm[cur][of * 2 + 1] = 0;
                                }
                            }
                        } else {
                            let mut lc = (u16::from(f(d)) << 4) | (u16::from(f(d + 2)) >> 4);
                            let mut rc = (u16::from(f(d + 1)) << 4) | (u16::from(f(d + 2)) & 0x0f);
                            lc = if lc == 0x800 { 0 } else { audio_12to16(lc) };
                            rc = if rc == 0x800 { 0 } else { audio_12to16(rc) };
                            let of = usize::from(sys.audio_shuffle[i % half_ch][j]) + (d - 8) / 3 * sys.audio_stride;
                            if of * 2 < size {
                                pcm[cur][of * 2] = (lc & 0xff) as u8;
                                pcm[cur][of * 2 + 1] = (lc >> 8) as u8;
                                let of = usize::from(sys.audio_shuffle[i % half_ch + half_ch][j]) + (d - 8) / 3 * sys.audio_stride;
                                if of * 2 + 1 < pcm[cur].len() {
                                    pcm[cur][of * 2] = (rc & 0xff) as u8;
                                    pcm[cur][of * 2 + 1] = (rc >> 8) as u8;
                                }
                                d += 1;
                            }
                        }
                        d += 2;
                    }
                    pos += 16 * 80;
                }
            }
        }
    }

    let mut pairs: Vec<Vec<i16>> = pcm
        .into_iter()
        .take(ach)
        .map(|b| b[..size].as_chunks::<2>().0.iter().map(|c| i16::from_le_bytes(*c)).collect())
        .collect();
    if sys.height == 720 {
        let clear: &[usize] = if frame[1] & 0x0c != 0 { &[2, 3] } else { &[0, 1] };
        for &c in clear {
            if let Some(p) = pairs.get_mut(c) {
                p.clear();
            }
        }
    }
    Some(DvAudio { sample_rate: sr, samples: size / 4, nonlinear_12bit: quant == 1, pairs })
}
