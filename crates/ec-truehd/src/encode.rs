//! A stereo, lossless TrueHD encoder.
//!
//! **Scope (r1)**: 44.1/48 kHz family stereo only, one substream, one
//! restart header per access unit (so no cross-AU predictor/parameter
//! state), no matrixing (0 primitive matrices — the stereo pair is written
//! as two independent, uncorrelated channels) and no FIR/IIR prediction
//! (filter order 0 on both channels): every sample is written raw, as a
//! 24-bit offset-binary residual (`codebook = 0`, `huff_lsbs = 24`,
//! `quant_step = 0`), which [`crate::decode::Core::read_block`] reconstructs
//! back to the exact sample with no predictor and no quantization. This
//! trades compression for the shortest bitstream that is still conformant:
//! every field this build's own decoder does not need to see change is left
//! at the restart header's own reset default (`Substream::new()`), so the
//! encoder only ever sets `params_present`/`restart_present`, block size,
//! and the per-channel codebook/`huff_lsbs` pair.
//!
//! **Restart header self-check**: the lossless-check byte a restart header
//! after the first carries is verified by the decoder against the samples
//! output by the *previous* restart interval ([`decode::Core::output`]'s
//! `lossless_check_data` fold); since every access unit here is its own
//! restart interval, this encoder folds each AU's own samples the same way
//! and carries that byte into the next AU's restart header.

use std::collections::VecDeque;

use ec_core::bitio::BitWriter;
use ec_core::error::{Error, Result};
use ec_core::frame::{ChannelLayout, Frame, SampleFormat};
use ec_core::packet::Packet;
use ec_core::registry::{AudioParameters, CodecId, CodecParameters, Encoder, MediaParameters};

use crate::decode::restart_checksum;
use crate::sync::MAJOR_SYNC_TRUEHD;

/// `0x31EA >> 1`: the restart header's 13-bit sync field, common to both
/// noise types (the 14th bit picks the type).
const RESTART_SYNC13: u32 = 0x31EA >> 1;
/// Offset-binary bias for a 24-bit residual with no predictor and no
/// quantization: `sample + 2^23`, matching
/// [`crate::decode::Substream::recompute_sign_offset`] at `codebook = 0`,
/// `huff_lsbs = 24`, `quant_step = 0`.
const SAMPLE_BIAS_24: i64 = 1 << 23;

/// XOR-fold a 32-bit word to 8 bits (same fold [`decode::Core::output`]
/// uses for the lossless-check byte).
fn xor_32_to_8(v: u32) -> u8 {
    (v ^ (v >> 8) ^ (v >> 16) ^ (v >> 24)) as u8
}

/// The major sync's own CRC-16 (poly `0x002D`, MSB-first table form,
/// matching ffmpeg's `av_crc_init(..., le=0, bits=16, poly=0x2D, ...)`):
/// this crate's own decoder does not validate it (see `sync.rs`'s module
/// docs — no public description of its generator was available to check
/// against), but a foreign decoder (ffmpeg's `mlp_parse.c`) does, so a
/// bitstream meant to round-trip through one needs a correct value.
fn mlp_crc16_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    for (i, entry) in table.iter_mut().enumerate() {
        let mut c = (i as i32) << 24;
        for _ in 0..8 {
            let overflow = c >> 31;
            c = (c << 1) ^ ((0x002D_i32 << 16) & overflow);
        }
        *entry = (c as u32).swap_bytes();
    }
    table
}

/// The major sync's checksum field (bytes 26..28, little-endian): a CRC-16
/// over `major_sync[0..24]`, XORed with the little-endian word at
/// `major_sync[24..26]`.
fn major_sync_checksum(major_sync: &[u8; 28]) -> u16 {
    let table = mlp_crc16_table();
    let mut crc: u32 = 0;
    for &b in &major_sync[..24] {
        crc = table[usize::from((crc as u8) ^ b)] ^ (crc >> 8);
    }
    (crc as u16) ^ u16::from_le_bytes([major_sync[24], major_sync[25]])
}

/// A stereo TrueHD encoder. See the module docs for scope.
#[derive(Debug)]
pub struct TrueHdEncoder {
    params: CodecParameters,
    sample_rate: u32,
    rate_code: u8,
    /// Samples per channel a full access unit carries (`40 << (rate_code &
    /// 7)`); the last, partial access unit may carry fewer.
    access_unit_size: usize,
    /// Interleaved stereo samples awaiting an access unit, true PCM range
    /// (matches [`crate::decode::TrueHdDecoder`]'s own S32 left-justified
    /// output once shifted right by 8).
    pending: Vec<i32>,
    packets: VecDeque<Vec<u8>>,
    /// Lossless-check byte for the *next* access unit's restart header,
    /// folded from the access unit just written; `0` (unchecked by the
    /// decoder) before the first one.
    next_check: u8,
    eof: bool,
}

impl TrueHdEncoder {
    /// An encoder for one 44.1/48 kHz-family stereo stream.
    pub fn new(sample_rate: u32) -> Result<TrueHdEncoder> {
        let (rate_code, access_unit_size) = match sample_rate {
            48_000 => (0u8, 40usize),
            96_000 => (1u8, 80usize),
            192_000 => (2u8, 160usize),
            44_100 => (8u8, 40usize),
            88_200 => (9u8, 80usize),
            176_400 => (10u8, 160usize),
            other => {
                return Err(Error::unsupported(
                    format!("TrueHD encoding at {other} Hz"),
                    "only the 44.1/48 kHz families are implemented",
                ));
            }
        };
        let mut params = CodecParameters::new(CodecId::TrueHd);
        params.media = MediaParameters::Audio(AudioParameters {
            sample_rate,
            layout: ChannelLayout::Stereo,
            format: Some(SampleFormat::S32),
            bits_per_sample: None,
        });
        Ok(TrueHdEncoder {
            params,
            sample_rate,
            rate_code,
            access_unit_size,
            pending: Vec::new(),
            packets: VecDeque::new(),
            next_check: 0,
            eof: false,
        })
    }

    /// Encodes one access unit from `samples` (interleaved stereo, `<=
    /// access_unit_size` per channel — a shorter slice writes a shorter,
    /// still fully self-contained, access unit).
    fn encode_access_unit(&mut self, samples: &[i32]) -> Vec<u8> {
        let n = samples.len() / 2;
        debug_assert!(n <= self.access_unit_size);

        let mut seg = BitWriter::new();
        seg.write_bit(true); // params_present
        seg.write_bit(true); // restart_present

        seg.write_bits(RESTART_SYNC13, 13);
        seg.write_bit(true); // noise_type (0x31EB); unused, no matrices read it
        seg.write_bits(0, 16); // output_timing, uninterpreted by the decoder
        seg.write_bits(0, 4); // min_channel
        seg.write_bits(1, 4); // max_channel
        seg.write_bits(1, 4); // max_matrix_channel
        seg.write_bits(0, 4); // noise_shift
        seg.write_bits(0, 23); // noisegen_seed
        seg.write_bits(0, 19); // reserved
        seg.write_bit(false); // data_check_present
        seg.write_bits(u32::from(self.next_check), 8); // lossless_check
        seg.write_bits(0, 16); // reserved
        seg.write_bits(0, 6); // ch_assign: matrix channel 0 -> output slot 0
        seg.write_bits(1, 6); // ch_assign: matrix channel 1 -> output slot 1
        let bit_size = seg.bit_len() - 2; // restart_checksum's own convention
        let crc = restart_checksum(seg.as_bytes(), bit_size);
        seg.write_bits(u32::from(crc), 8);

        seg.write_bit(false); // presence-flags gate: keep the restart's 0xFF default
        seg.write_bit(true); // blocksize gate
        seg.write_bits(n as u32, 9);
        seg.write_bit(false); // matrix gate: keep 0 matrices
        seg.write_bit(false); // output-shift gate: keep shift 0
        seg.write_bit(false); // quant-step gate: keep step 0
        for _ch in 0..2 {
            seg.write_bit(true); // per-channel gate
            seg.write_bit(false); // FIR gate: keep order 0
            seg.write_bit(false); // IIR gate: keep order 0
            seg.write_bit(false); // huff_offset gate: keep 0
            seg.write_bits(0, 2); // codebook 0: raw, no Huffman code
            seg.write_bits(24, 5); // huff_lsbs: full 24-bit raw residual
        }

        for &sample in samples {
            let raw = ((i64::from(sample) + SAMPLE_BIAS_24) as u32) & 0x00FF_FFFF;
            seg.write_bits(raw, 24);
        }

        seg.write_bit(true); // end_of_segment
        seg.align_to_byte();
        if seg.bit_len() % 16 != 0 {
            seg.write_bits(0, 8);
        }
        let seg_bytes = seg.into_bytes();

        // Fold this AU's own samples the way `decode::Core::output` does,
        // for the *next* AU's restart header.
        let mut fold = 0u32;
        for (i, &sample) in samples.iter().enumerate() {
            fold ^= ((sample as u32) & 0x00FF_FFFF) << (i % 2);
        }
        self.next_check = xor_32_to_8(fold);

        // A foreign decoder (ffmpeg's mlp_parse.c) reads more of the major
        // sync than this crate's own decoder does: the two 13/5-bit
        // "channel arrangement" fields (`ch_arr`/`ch_arr2`, both set to a
        // stereo-only chanmap of 1 so `mlpdec.c` reports AV_CH_LAYOUT_STEREO
        // and `group1_bits == 24` so it decodes at S32) and `num_substreams`
        // (4 bits, at byte 16's top nibble) — 1, matching our own single-
        // substream directory.
        let mut major_sync = [0u8; 28];
        major_sync[0..4].copy_from_slice(&MAJOR_SYNC_TRUEHD.to_be_bytes());
        major_sync[4] = self.rate_code << 4; // low nibble ignored by both decoders
        major_sync[5] = 0x00;
        major_sync[6] = 0x80; // ch_arr low bit (=1) + ch_arr2 top 5 bits (0)
        major_sync[7] = 0x01; // ch_arr2 low 8 bits (=1)
        major_sync[16] = 0x10; // num_substreams = 1
        let checksum = major_sync_checksum(&major_sync);
        major_sync[26..28].copy_from_slice(&checksum.to_le_bytes());
        let dir_word = (seg_bytes.len() as u16 / 2) & 0x0FFF; // 1 substream, no extra word

        let total_len = 4 + major_sync.len() + 2 + seg_bytes.len();
        let len_words = (total_len as u16 / 2) & 0x0FFF;
        let dir_bytes = dir_word.to_be_bytes();
        // Not "unused" after all: a foreign decoder (ffmpeg's mlpdec.c,
        // `ff_mlp_calculate_parity`) folds the AU header's 4 bytes against
        // the substream directory's bytes and rejects the AU unless the
        // result's two nibbles are bitwise complements. This crate's own
        // decoder never checks it (`sync.rs`'s docs), so the nibble is free
        // to pick; find the one value (of 16) that satisfies the fold.
        let check_nibble = (0u8..16)
            .find(|&cn| {
                let b0 = (cn << 4) | ((len_words >> 8) as u8 & 0x0F);
                let b1 = (len_words & 0xFF) as u8;
                let p = b0 ^ b1 ^ dir_bytes[0] ^ dir_bytes[1];
                ((p >> 4) ^ p) & 0xF == 0xF
            })
            .expect("one of the 16 check-nibble values always satisfies the fold");
        let length_word = (u16::from(check_nibble) << 12) | len_words;

        let mut au = Vec::with_capacity(total_len);
        au.extend_from_slice(&length_word.to_be_bytes());
        au.extend_from_slice(&0u16.to_be_bytes()); // input_timing
        au.extend_from_slice(&major_sync);
        au.extend_from_slice(&dir_bytes);
        au.extend_from_slice(&seg_bytes);
        au
    }

    fn drain_full_units(&mut self) {
        let per_au = self.access_unit_size * 2;
        while self.pending.len() >= per_au {
            let samples: Vec<i32> = self.pending.drain(..per_au).collect();
            let bytes = self.encode_access_unit(&samples);
            self.packets.push_back(bytes);
        }
    }
}

impl Encoder for TrueHdEncoder {
    fn codec_parameters(&self) -> &CodecParameters {
        &self.params
    }

    fn send_frame(&mut self, frame: &Frame) -> Result<()> {
        let Frame::Audio(audio) = frame else {
            return Err(Error::corrupt("video frame pushed into a TrueHD encoder"));
        };
        if audio.format != SampleFormat::S32 {
            return Err(Error::unsupported(
                format!("{:?} input", audio.format),
                "this encoder takes S32 (24-bit left-justified) samples",
            ));
        }
        if audio.layout.channel_count() != 2 {
            return Err(Error::unsupported(
                format!("{} channel input", audio.layout.channel_count()),
                "this encoder writes stereo only",
            ));
        }
        if audio.rate != self.sample_rate {
            return Err(Error::unsupported(
                format!("{} Hz input", audio.rate),
                "this encoder was set up for a different sample rate",
            ));
        }
        if audio.planar {
            for i in 0..audio.samples {
                for plane in audio.data.iter().take(2) {
                    let at = i * 4;
                    let w = [plane[at], plane[at + 1], plane[at + 2], plane[at + 3]];
                    self.pending.push(i32::from_ne_bytes(w) >> 8);
                }
            }
        } else {
            for word in audio.data[0].chunks_exact(4).take(audio.samples * 2) {
                self.pending
                    .push(i32::from_ne_bytes([word[0], word[1], word[2], word[3]]) >> 8);
            }
        }
        self.drain_full_units();
        Ok(())
    }

    fn receive_packet(&mut self) -> Result<Packet> {
        match self.packets.pop_front() {
            Some(data) => Ok(Packet::new(
                0,
                ec_core::timebase::TimeBase::new(1, i64::from(self.sample_rate)),
                data,
            )),
            None if self.eof => Err(Error::Eof),
            None => Err(Error::NeedMore),
        }
    }

    fn flush(&mut self) -> Result<()> {
        if !self.pending.is_empty() {
            let mut samples = std::mem::take(&mut self.pending);
            // corner-cut: TrueHD's own block-size field is invalid under 8
            // samples/channel (crate::decode::read_decoding_params rejects
            // it); a final partial access unit shorter than that is padded
            // by repeating its last real sample pair. Ceiling: a caller
            // wanting the exact original count trims the decoder's output
            // to it (the container's duration, same as any codec's short
            // final frame) — upgrade path is a real short-block encoding,
            // not needed at r1 scope.
            if samples.len() / 2 < 8 {
                let last = [
                    *samples.get(samples.len().wrapping_sub(2)).unwrap_or(&0),
                    *samples.last().unwrap_or(&0),
                ];
                while samples.len() / 2 < 8 {
                    samples.extend_from_slice(&last);
                }
            }
            let bytes = self.encode_access_unit(&samples);
            self.packets.push_back(bytes);
        }
        self.eof = true;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::TrueHdDecoder;
    use ec_core::frame::{AudioFrame, Frame as EcFrame};
    use ec_core::packet::Buf;

    /// A deterministic tone+noise fixture, interleaved stereo S32
    /// left-justified, `n` samples per channel.
    fn tone_fixture(n: usize) -> Vec<i32> {
        let mut v = Vec::with_capacity(n * 2);
        let mut lcg: u32 = 0x12345;
        for i in 0..n {
            let l = ((i as f64 * 0.07).sin() * 4_000_000.0) as i32;
            lcg = lcg.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let r = l.wrapping_add((lcg >> 8) as i32 % 5000 - 2500);
            v.push(l.clamp(-8_388_608, 8_388_607));
            v.push(r.clamp(-8_388_608, 8_388_607));
        }
        v
    }

    fn s32_frame(interleaved: &[i32], sample_rate: u32) -> ec_core::frame::AudioFrame {
        let mut bytes = Vec::with_capacity(interleaved.len() * 4);
        for &s in interleaved {
            bytes.extend_from_slice(&(s << 8).to_ne_bytes());
        }
        AudioFrame::try_new(
            SampleFormat::S32,
            false,
            ChannelLayout::Stereo,
            sample_rate,
            interleaved.len() / 2,
            vec![Buf::from_vec(bytes)],
        )
        .unwrap()
    }

    /// Round-trip gate 1: encode a multi-access-unit stereo fixture (a
    /// partial final access unit included), decode every access unit back
    /// through this crate's own decoder, and check every sample exactly.
    #[test]
    fn encoder_round_trips_through_our_own_decoder() {
        for n in [173usize, 40, 40 * 5 + 7, 1] {
            let samples = tone_fixture(n);
            let mut enc = TrueHdEncoder::new(48_000).unwrap();
            enc.send_frame(&EcFrame::Audio(s32_frame(&samples, 48_000)))
                .unwrap();
            enc.flush().unwrap();

            let mut dec = TrueHdDecoder::new();
            let mut decoded = Vec::new();
            loop {
                match enc.receive_packet() {
                    Ok(pkt) => {
                        if let Some(frame) = dec.decode_access_unit(&pkt.data).unwrap() {
                            for word in frame.data[0].chunks_exact(4) {
                                decoded.push(
                                    i32::from_ne_bytes([word[0], word[1], word[2], word[3]]) >> 8,
                                );
                            }
                        }
                    }
                    Err(Error::Eof) => break,
                    Err(e) => panic!("n={n}: {e:?}"),
                }
            }
            decoded.truncate(samples.len());
            assert_eq!(decoded, samples, "n={n}: sample mismatch");
            let stats = dec.check_stats();
            assert_eq!(stats.restart_crc_failures, 0, "n={n}: restart CRC");
            assert_eq!(stats.lossless_check_failures, 0, "n={n}: lossless check");
            assert_eq!(stats.parity_failures, 0, "n={n}: parity");
            assert_eq!(stats.length_mismatches, 0, "n={n}: length mismatch");
        }
    }
}
