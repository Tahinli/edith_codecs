//! AMR-WB (3GPP TS 26.173) fixed-point decoder.
#![allow(
    clippy::all,
    clippy::pedantic,
    dead_code,
    unused_assignments,
    non_snake_case,
    non_upper_case_globals
)]

pub mod bits;
pub mod core;
pub mod dtx;
pub mod exc;
pub mod exc_tab;
pub mod isf;
pub mod isf_tab;
pub mod math;
pub mod ops;
pub mod synth;

use ec_core::error::{Error, Result};
use ec_core::frame::{AudioFrame, ChannelLayout, Frame, SampleFormat};
use ec_core::packet::{Buf, Packet};
use ec_core::registry::{AudioParameters, CodecId, CodecParameters, Decoder, MediaParameters};

use self::bits::{
    RxState, mime_unsorting, pvDecoder_AmrWb_homing_frame_test,
    pvDecoder_AmrWb_homing_frame_test_first,
};
use self::core::{DecoderState, decode_frame};
use self::dtx::{RX_NO_DATA, RX_SPEECH_GOOD, RX_SPEECH_LOST};

/// Samples in one decoded AMR-WB frame (20 ms at 16 kHz).
pub const FRAME_SAMPLES: usize = 320;

/// Payload bytes (after the ToC byte) of an AMR-WB frame type, `None` for reserved types.
fn payload_len(ft: u8) -> Option<usize> {
    const SIZES: [usize; 10] = [17, 23, 32, 36, 40, 46, 50, 58, 60, 5];
    match ft {
        0..=9 => Some(SIZES[usize::from(ft)]),
        14 | 15 => Some(0),
        _ => None,
    }
}

/// A fixed-point AMR-WB decoder (3GPP TS 26.173), bit-exact with the reference decoder's
/// 14-bit output. Packets are one or more frames, each a ToC byte followed by the speech bits
/// (the `sawb` sample layout and the `.awb` frame layout).
pub struct AmrWbDecoder {
    st: Box<DecoderState>,
    rx: RxState,
    mode_old: i16,
    reset_flag: i16,
    reset_flag_old: i16,
    params: CodecParameters,
    out: Vec<i16>,
    pending: Option<AudioFrame>,
    drained: bool,
}

impl Default for AmrWbDecoder {
    fn default() -> Self {
        AmrWbDecoder::new()
    }
}

impl AmrWbDecoder {
    /// A decoder in its initial state.
    pub fn new() -> AmrWbDecoder {
        let mut params = CodecParameters::new(CodecId::AmrWb);
        params.media = MediaParameters::Audio(AudioParameters {
            sample_rate: 16000,
            layout: ChannelLayout::Mono,
            format: Some(SampleFormat::S16),
            bits_per_sample: Some(16),
        });
        AmrWbDecoder {
            st: DecoderState::new(),
            rx: RxState {
                prev_ft: RX_SPEECH_GOOD,
                prev_mode: 0,
            },
            mode_old: 0,
            reset_flag: 0,
            reset_flag_old: 1,
            params,
            out: Vec::new(),
            pending: None,
            drained: false,
        }
    }

    /// A decoder for a stream described by `params` (the `damr` extradata, if any, is kept).
    pub fn from_parameters(params: CodecParameters) -> Result<AmrWbDecoder> {
        let mut d = AmrWbDecoder::new();
        d.params.extradata = params.extradata;
        Ok(d)
    }

    fn reset_state(&mut self) {
        self.st = DecoderState::new();
        self.rx = RxState {
            prev_ft: RX_SPEECH_GOOD,
            prev_mode: 0,
        };
        self.mode_old = 0;
        self.reset_flag = 0;
        self.reset_flag_old = 1;
    }

    /// Decode the frames of one packet; the slice is valid until the next call. Each frame
    /// contributes 320 samples at 16 kHz.
    pub fn decode(&mut self, data: &[u8]) -> Result<&[i16]> {
        self.out.clear();
        let mut at = 0;
        while at < data.len() {
            let toc = data[at];
            let ft = (toc >> 3) & 0x0F;
            let n = payload_len(ft)
                .ok_or_else(|| Error::corrupt(format!("AMR-WB frame type {ft} is reserved")))?;
            if at + 1 + n > data.len() {
                return Err(Error::corrupt("AMR-WB frame is truncated"));
            }
            let mut buf = [0u8; 64];
            buf[..n].copy_from_slice(&data[at + 1..at + 1 + n]);
            let quality = (toc >> 2) & 1;
            self.decode_frame(ft as i16, quality, &buf);
            at += 1 + n;
        }
        Ok(&self.out)
    }

    /// `D_IF_decode` of the reference interface.
    fn decode_frame(&mut self, mode_in: i16, quality: u8, payload: &[u8]) {
        let mut mode = mode_in;
        let mut frame_type = RX_SPEECH_GOOD;
        let mut input = [0i16; 480];
        mime_unsorting(
            payload,
            &mut input,
            &mut frame_type,
            &mut mode,
            quality,
            &mut self.rx,
        );

        if frame_type == RX_NO_DATA || frame_type == RX_SPEECH_LOST {
            mode = self.mode_old;
            self.reset_flag = 0;
        } else {
            self.mode_old = mode;
            if self.reset_flag_old == 1 {
                self.reset_flag = pvDecoder_AmrWb_homing_frame_test_first(&input, mode);
            }
        }

        let mut synth = [0i16; FRAME_SAMPLES];
        if self.reset_flag != 0 && self.reset_flag_old != 0 {
            synth.fill(ops::EHF_MASK);
        } else {
            decode_frame(mode, &input, &mut synth, &mut self.st, frame_type);
        }
        for s in synth.iter_mut() {
            *s &= !3;
        }
        if self.reset_flag_old == 0 {
            self.reset_flag = pvDecoder_AmrWb_homing_frame_test(&input, mode);
        }
        if self.reset_flag != 0 {
            self.st.reset(true);
        }
        self.reset_flag_old = self.reset_flag;
        self.out.extend_from_slice(&synth);
    }
}

impl Decoder for AmrWbDecoder {
    fn codec_parameters(&self) -> &CodecParameters {
        &self.params
    }

    fn send_packet(&mut self, packet: &Packet) -> Result<()> {
        self.decode(&packet.data)?;
        let mut bytes = Vec::with_capacity(self.out.len() * 2);
        for &s in &self.out {
            bytes.extend_from_slice(&s.to_ne_bytes());
        }
        let mut frame = AudioFrame::try_new(
            SampleFormat::S16,
            false,
            ChannelLayout::Mono,
            16000,
            self.out.len(),
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
        self.pending = None;
        self.drained = false;
        self.reset_state();
    }
}
